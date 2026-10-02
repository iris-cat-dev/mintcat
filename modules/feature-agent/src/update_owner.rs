use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use mintcat_runtime_artifacts::ArtifactStore;
use mintcat_runtime_updates::{OwnerBackup, OwnerFuture, Result, ScopeGeneration, StateOwner, UpdateError};
use mintcat_sdk::{ModuleComponent, ModuleManifest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{AgentError, NativeAgentSessions, AGENT_CONTRACT_ID, AGENT_CONTRACT_VERSION};

const OWNER_ID: &str = "mintcat.feature.agent";
const SCHEMA: u64 = 1;

/// Update adapter for the actual signed Agent process. Consumers retain the shared facade;
/// only its paused native backend changes. The host never opens the Agent database.
pub struct NativeAgentStateOwner {
    agent: Arc<NativeAgentSessions>,
    backups: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotReceipt {
    format_version: u64,
    schema_version: u64,
    snapshot_name: String,
    sha256: String,
    write_epoch: u64,
    owner_digest: Option<String>,
    absent: bool,
    database_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AbsenceProof {
    format_version: u64,
    schema_version: u64,
    absent: bool,
    write_epoch: u64,
    database_path: PathBuf,
}

fn require(condition: bool, message: &'static str) -> Result<()> {
    if condition { Ok(()) } else { Err(UpdateError::Blocked(message.into())) }
}

impl NativeAgentStateOwner {
    pub fn new(agent: Arc<NativeAgentSessions>, backup_dir: PathBuf) -> Self {
        Self { agent, backups: backup_dir }
    }

    fn owner_error(error: AgentError) -> UpdateError {
        if error == AgentError::Busy {
            UpdateError::Busy("Agent turns must drain before native owner maintenance".into())
        } else {
            // Only the typed, sanitized code crosses the update diagnostics boundary.
            UpdateError::Blocked(format!("Agent native owner: {}", error.code()))
        }
    }

    fn prepare_backup_directory(&self) -> Result<()> {
        fs::create_dir_all(&self.backups)?;
        require(fs::symlink_metadata(&self.backups)?.file_type().is_dir(),
            "Agent backup directory must be a real directory")?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.backups, fs::Permissions::from_mode(0o700))?;
        }
        File::open(&self.backups)?.sync_all()?;
        if let Some(parent) = self.backups.parent().filter(|p| !p.as_os_str().is_empty()) {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    fn snapshot_file(path: &Path) -> Result<File> {
        let metadata = fs::symlink_metadata(path)?;
        require(metadata.file_type().is_file(), "Agent snapshot must be a regular file")?;
        let file = File::open(path)?;
        let opened = file.metadata()?;
        require(opened.is_file(), "Agent snapshot must be a regular file")?;
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            require(metadata.dev() == opened.dev() && metadata.ino() == opened.ino()
                && opened.nlink() == 1 && opened.mode() & 0o077 == 0,
                "Agent snapshot must be a private, unlinked regular file")?;
        }
        Ok(file)
    }

    fn digest(file: &mut File) -> Result<String> {
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 { break; }
            hash.update(&buffer[..count]);
        }
        Ok(format!("sha256:{:x}", hash.finalize()))
    }

    fn receipt(&self, backup: &OwnerBackup) -> Result<SnapshotReceipt> {
        require(backup.owner_id == OWNER_ID && backup.old_schema == SCHEMA && backup.new_schema == SCHEMA,
            "Agent backup belongs to another owner or schema")?;
        let receipt: SnapshotReceipt = serde_json::from_value(backup.reference.clone())
            .map_err(|_| UpdateError::Blocked("Agent snapshot receipt is invalid".into()))?;
        require(receipt.format_version == 1 && receipt.schema_version == SCHEMA,
            "Agent snapshot receipt format or schema is unsupported")?;
        require(receipt.database_path == self.agent.database_path(),
            "Agent snapshot belongs to another database path")?;
        require(if receipt.absent { receipt.write_epoch == 0 && receipt.owner_digest.is_none() }
            else { receipt.owner_digest.is_some() }, "Agent snapshot owner identity is invalid")?;
        let suffix = if receipt.absent { ".json" } else { ".sqlite" };
        let name = receipt.snapshot_name.strip_suffix(suffix)
            .ok_or_else(|| UpdateError::Blocked("Agent snapshot name is invalid".into()))?;
        let id = uuid::Uuid::parse_str(name)
            .map_err(|_| UpdateError::Blocked("Agent snapshot name is invalid".into()))?;
        require(id.to_string() == name, "Agent snapshot name is invalid")?;
        Ok(receipt)
    }

    fn replace_generation(&self, generation: &ScopeGeneration, artifacts: ArtifactStore) -> Result<()> {
        let mut packages = generation.targets.signed.packages.iter()
            .filter(|(_, package)| package.manifest.id == OWNER_ID);
        let (digest, package) = packages.next()
            .ok_or_else(|| UpdateError::Blocked("generation has no signed Agent native owner".into()))?;
        require(packages.next().is_none(), "generation has multiple Agent native owners")?;
        self.supports(None, &package.manifest)?;
        require(package.digest == *digest, "Agent package digest differs from signed generation")?;
        let installed = artifacts.load_module(digest)?;
        require(installed.manifest == package.manifest && installed.signature == package.signature,
            "Agent installed package differs from exact signed generation")?;
        let root = mintcat_runtime_release::trust_pinned_root(generation.verified_root.clone(), 0)?;
        root.verify("targets", &generation.targets)?;
        root.verify_package_signature(&installed.manifest, &installed.signature,
            &mintcat_runtime_artifacts::module_manifest_signing_digest(&installed.manifest)?)?;
        self.agent.replace_native(artifacts, digest).map_err(Self::owner_error)?;
        require(self.agent.owner_digest().map_err(Self::owner_error)? == *digest,
            "Agent process identity differs from requested signed generation")?;
        self.agent.read_only_health().map_err(Self::owner_error)
    }
}

impl StateOwner for NativeAgentStateOwner {
    fn owner_id(&self) -> &str { OWNER_ID }

    fn supports(&self, old: Option<&ModuleManifest>, new: &ModuleManifest) -> Result<()> {
        require(new.id == OWNER_ID && new.state.schema_version == SCHEMA
            && old.is_none_or(|m| m.id == OWNER_ID && m.state.schema_version == SCHEMA),
            "Agent native owner supports schema1 only; state migration is not implemented")?;
        require(new.components.iter().filter(|c| self.handles_native(new, c)).count() == 1
            && new.components.iter().all(|c| c.state_model == "stateless" || self.handles_native(new, c)),
            "Agent update requires exactly one native service worker single writer and agent.owner v1")
    }

    fn handles_native(&self, manifest: &ModuleManifest, component: &ModuleComponent) -> bool {
        manifest.id == OWNER_ID && manifest.state.schema_version == SCHEMA
            && component.kind == "service" && component.runtime == "worker"
            && component.state_model == "single-writer"
            && manifest.provides.iter().any(|p| p.contract_id == AGENT_CONTRACT_ID
                && p.version == AGENT_CONTRACT_VERSION)
    }

    fn prepare_code<'a>(&'a self, generation: &'a ScopeGeneration, artifacts: ArtifactStore) -> OwnerFuture<'a, ()> {
        Box::pin(async move { self.replace_generation(generation, artifacts) })
    }

    fn rollback_code<'a>(&'a self, generation: &'a ScopeGeneration, artifacts: ArtifactStore) -> OwnerFuture<'a, ()> {
        Box::pin(async move { self.replace_generation(generation, artifacts) })
    }

    fn deactivate_code<'a>(&'a self, generation: &'a ScopeGeneration) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            let mut packages = generation.targets.signed.packages.iter()
                .filter(|(_, package)| package.manifest.id == OWNER_ID);
            let (digest, package) = packages.next()
                .ok_or_else(|| UpdateError::Blocked("generation has no Agent owner to deactivate".into()))?;
            require(packages.next().is_none(), "generation has multiple Agent native owners")?;
            self.supports(None, &package.manifest)?;
            if self.agent.has_native().map_err(Self::owner_error)? {
                require(self.agent.owner_digest().map_err(Self::owner_error)? == *digest,
                    "Agent deactivation would deselect another signed generation")?;
            }
            // The facade pauses, reaps the process and becomes unavailable. It retains
            // every database and epoch file; an absent predecessor is not a data restore.
            self.agent.deactivate_native().map_err(Self::owner_error)
        })
    }

    fn drain(&self) -> OwnerFuture<'_, ()> {
        Box::pin(async {
            if self.agent.is_pristine_absent().map_err(Self::owner_error)? { return Ok(()); }
            self.agent.pause().map_err(Self::owner_error)
        })
    }

    fn backup<'a>(&'a self, old_schema: u64, new_schema: u64) -> OwnerFuture<'a, OwnerBackup> {
        Box::pin(async move {
            require(old_schema == SCHEMA && new_schema == SCHEMA,
                "Agent cross-schema backup requires an implemented state migration provider")?;
            self.prepare_backup_directory()?;
            if self.agent.is_pristine_absent().map_err(Self::owner_error)? {
                let proof = AbsenceProof {
                    format_version: 1, schema_version: SCHEMA, absent: true,
                    write_epoch: 0, database_path: self.agent.database_path().to_path_buf(),
                };
                let snapshot_name = format!("{}.json", uuid::Uuid::new_v4());
                let path = self.backups.join(&snapshot_name);
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)] {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&path)?;
                serde_json::to_writer(&mut file, &proof)?;
                file.sync_all()?;
                drop(file);
                let mut file = Self::snapshot_file(&path)?;
                let sha256 = Self::digest(&mut file)?;
                File::open(&self.backups)?.sync_all()?;
                require(self.agent.is_pristine_absent().map_err(Self::owner_error)?,
                    "Agent state appeared during absent-state backup")?;
                return Ok(OwnerBackup {
                    owner_id: OWNER_ID.into(), old_schema, new_schema,
                    reference: serde_json::to_value(SnapshotReceipt {
                        format_version: 1, schema_version: SCHEMA, snapshot_name, sha256,
                        write_epoch: 0, owner_digest: None, absent: true,
                        database_path: proof.database_path,
                    })?,
                });
            }
            let owner_digest = self.agent.owner_digest().map_err(Self::owner_error)?;
            let write_epoch = self.agent.write_epoch().map_err(Self::owner_error)?;
            let snapshot_name = format!("{}.sqlite", uuid::Uuid::new_v4());
            let path = self.backups.join(&snapshot_name);
            self.agent.backup_to(&path).map_err(Self::owner_error)?;
            let mut snapshot = Self::snapshot_file(&path)?;
            snapshot.sync_all()?;
            let sha256 = Self::digest(&mut snapshot)?;
            File::open(&self.backups)?.sync_all()?;
            require(self.agent.write_epoch().map_err(Self::owner_error)? == write_epoch
                && self.agent.owner_digest().map_err(Self::owner_error)? == owner_digest,
                "Agent owner changed during paused snapshot creation")?;
            Ok(OwnerBackup {
                owner_id: OWNER_ID.into(), old_schema, new_schema,
                reference: serde_json::to_value(SnapshotReceipt {
                    format_version: 1, schema_version: SCHEMA, snapshot_name, sha256,
                    write_epoch, owner_digest: Some(owner_digest), absent: false,
                    database_path: self.agent.database_path().to_path_buf(),
                })?,
            })
        })
    }

    fn migrate<'a>(&'a self, _: &'a OwnerBackup) -> OwnerFuture<'a, ()> {
        Box::pin(async {
            Err(UpdateError::Blocked("Agent schema1 native activation is not a state migration; cross-schema migration is unsupported".into()))
        })
    }

    fn read_only_health(&self) -> OwnerFuture<'_, ()> {
        Box::pin(async { self.agent.read_only_health().map_err(Self::owner_error) })
    }

    fn restore<'a>(&'a self, backup: &'a OwnerBackup) -> OwnerFuture<'a, ()> {
        Box::pin(async move {
            let receipt = self.receipt(backup)?;
            let path = self.backups.join(&receipt.snapshot_name);
            let mut snapshot = Self::snapshot_file(&path)?;
            require(Self::digest(&mut snapshot)? == receipt.sha256, "Agent snapshot digest mismatch")?;
            if receipt.absent {
                snapshot.seek(SeekFrom::Start(0))?;
                let proof: AbsenceProof = serde_json::from_reader(snapshot)
                    .map_err(|_| UpdateError::Blocked("Agent absence proof is invalid".into()))?;
                require(proof.format_version == 1 && proof.schema_version == SCHEMA
                    && proof.absent && proof.write_epoch == 0
                    && proof.database_path == receipt.database_path,
                    "Agent absence proof does not match its receipt")?;
                if self.agent.is_pristine_absent().map_err(Self::owner_error)? { return Ok(()); }
                // A launched candidate may have an empty database, but no owner writes may
                // have occurred. Never delete state files to recreate the absent condition.
                self.agent.pause().map_err(Self::owner_error)?;
                require(self.agent.write_epoch().map_err(Self::owner_error)? == 0
                    && !self.agent.has_nonempty().map_err(Self::owner_error)?,
                    "Agent absent-state rollback forbidden after owner state appeared")?;
                return self.agent.read_only_health().map_err(Self::owner_error);
            }
            require(self.agent.owner_digest().map_err(Self::owner_error)?.as_str()
                == receipt.owner_digest.as_deref().unwrap_or_default(),
                "Agent restore requires the exact previous signed owner process")?;
            require(self.agent.write_epoch().map_err(Self::owner_error)? == receipt.write_epoch,
                "Agent rollback forbidden after first new owner write; preserve live data")?;
            // The worker checks the persistent epoch again under its exclusive writer lock.
            // This host check cannot substitute for that crash-safe independent restore fence.
            self.agent.restore_from(&path, receipt.write_epoch).map_err(Self::owner_error)?;
            require(self.agent.write_epoch().map_err(Self::owner_error)? == receipt.write_epoch,
                "Agent restoration changed its durable write fence")?;
            self.agent.read_only_health().map_err(Self::owner_error)
        })
    }

    fn open_writes(&self) -> OwnerFuture<'_, ()> {
        Box::pin(async { self.agent.open_writes().map_err(Self::owner_error) })
    }
}
