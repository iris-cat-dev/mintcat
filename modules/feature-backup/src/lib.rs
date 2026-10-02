mod maintenance;
mod cloud;
mod native;
mod owners;
pub use owners::{OwnerSnapshot, PortableSnapshot, PortableOwners};
pub use native::{NativeBackupCodec, module_manifest, execute_codec, worker_workspace, BACKUP_CONTRACT_ID, BACKUP_CONTRACT_VERSION};
pub use maintenance::{MaintenanceAdmission, MaintenanceGate, MaintenanceMarker, MaintenancePhase};
pub use cloud::{CloudBackupRecord, CloudBackupTransport, CloudMetadata, CloudCredentialProvider};

use std::{collections::BTreeMap, fs::{self, File, OpenOptions}, io::Write, path::{Path, PathBuf}};
use mintcat_library_service::{LibraryCounts, LibraryError, LibrarySnapshot, LibraryStore};
use mintcat_runtime_kernel::RuntimeLayout;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub const BACKUP_FORMAT_VERSION: u32 = 2;
pub const LIBRARY_SCHEMA_VERSION: u32 = 5;
#[derive(Debug, Error)]
pub enum BackupError {
    #[error(transparent)] Io(#[from] std::io::Error),
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error(transparent)] Library(#[from] LibraryError),
    #[error("invalid backup: {0}")] Invalid(String),
    #[error("backup maintenance blocked: {0}")] Busy(String),
    #[error("backup maintenance requires explicit recovery")] RecoveryRequired,
    #[error("backup module unavailable or incompatible: {0}")] Module(String),
    #[error("cloud backup failed: {0}")] Cloud(String),
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleVersion { pub version: String, pub schema_version: u32 }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupManifest {
    pub format_version: u32,
    pub owner_id: String,
    pub owner_schema_version: u32,
    pub module_versions: BTreeMap<String, ModuleVersion>,
    pub credential_policy: String,
    pub content_digest: String,
    pub content_size: u64,
    pub consistency: String,
    pub owner_schemas: BTreeMap<String, u32>,
    pub excluded_authority: Vec<String>,
}
/// `content` is the exact UTF-8 owner snapshot bytes; digest is not computed over a reserialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupArchive { pub manifest: BackupManifest, pub content: String }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportBackupRequest { pub target_path: PathBuf }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreBackupRequest { pub source_path: PathBuf }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupResult { pub manifest: BackupManifest, pub counts: LibraryCounts }
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupBlockers {
    pub active_jobs: Vec<String>,
    pub unfinished_install_jobs: Vec<String>,
    pub file_journals: Vec<String>,
    pub module_transitions: Vec<String>,
    pub nonempty_owners: Vec<String>,
}
impl BackupBlockers {
    fn ensure_quiescent(&self, restore: bool) -> Result<(), BackupError> {
        if !self.active_jobs.is_empty() || !self.unfinished_install_jobs.is_empty() || !self.file_journals.is_empty() || !self.module_transitions.is_empty() || (restore && !self.nonempty_owners.is_empty()) {
            return Err(BackupError::Busy(serde_json::to_string(self)?));
        }
        Ok(())
    }
}
/// Parent inspector runs under the exclusive admission lock, never before it.
/// It reads owner APIs for jobs, journals, install state and the installed module closure.
#[derive(Debug, Clone)]
pub struct BackupRuntimeState { pub blockers: BackupBlockers, pub modules: BTreeMap<String, ModuleVersion> }
pub struct BackupService { layout: RuntimeLayout, library: LibraryStore, codec: Option<std::sync::Arc<NativeBackupCodec>>, owners: Option<std::sync::Arc<dyn PortableOwners>> }
impl BackupService {
    pub fn new(layout: RuntimeLayout, library: LibraryStore) -> Result<Self, BackupError> {
        if library.database_path() != layout.state_dir.join("mintcat.library/library.sqlite") {
            return Err(BackupError::Invalid("Library owner belongs to a different data directory".into()));
        }
        Ok(Self { layout, library, codec: None, owners: None })
    }
    pub fn with_native_backend(layout: RuntimeLayout, library: LibraryStore, codec: std::sync::Arc<NativeBackupCodec>) -> Result<Self, BackupError> {
        let mut service = Self::new(layout,library)?;
        service.codec = Some(codec);
        Ok(service)
    }
    pub fn with_owners(mut self, owners: std::sync::Arc<dyn PortableOwners>) -> Self { self.owners=Some(owners); self }
    fn snapshot(&self)->Result<PortableSnapshot,BackupError> {
        let mut snapshot=PortableSnapshot {library:self.library.backup_snapshot()?,owners:match &self.owners {Some(owners)=>owners.snapshot()?,None=>BTreeMap::new()}};
        snapshot.exclude_execution_authority()?;
        Ok(snapshot)
    }
    pub fn export(&self, request: ExportBackupRequest, inspect: impl FnOnce() -> Result<BackupRuntimeState, BackupError>) -> Result<BackupResult, BackupError> {
        let (bytes, result) = self.export_archive(inspect)?;
        atomic_write(&request.target_path, &bytes, false)?;
        Ok(result)
    }
    pub fn export_bytes(&self, inspect: impl FnOnce() -> Result<BackupRuntimeState, BackupError>) -> Result<Vec<u8>, BackupError> {
        let (bytes, _) = self.export_archive(inspect)?;
        Ok(bytes)
    }
    pub fn export_archive(&self, inspect: impl FnOnce() -> Result<BackupRuntimeState, BackupError>) -> Result<(Vec<u8>, BackupResult), BackupError> {
        let _gate = MaintenanceGate::exclusive(&self.layout, false)?;
        let state = inspect()?;
        state.blockers.ensure_quiescent(false)?;
        let snapshot = self.snapshot()?;
        match &self.codec {
            Some(codec) => codec.encode(&snapshot,&state.modules),
            None => encode_archive(&snapshot,state.modules),
        }
    }
    pub fn restore(&self, request: RestoreBackupRequest, inspect: impl FnOnce() -> Result<BackupRuntimeState, BackupError>) -> Result<BackupResult, BackupError> {
        self.restore_bytes(&fs::read(request.source_path)?, inspect)
    }
    pub fn restore_bytes(&self, bytes: &[u8], inspect: impl FnOnce() -> Result<BackupRuntimeState, BackupError>) -> Result<BackupResult, BackupError> {
        self.restore_bytes_for_operation(bytes,None,inspect)
    }
    pub fn restore_bytes_for_operation(&self,bytes:&[u8],operation_id:Option<&str>,inspect:impl FnOnce()->Result<BackupRuntimeState,BackupError>)->Result<BackupResult,BackupError> {
        let gate = MaintenanceGate::exclusive(&self.layout, false)?;
        let state = inspect()?;
        state.blockers.ensure_quiescent(true)?;
        self.library.ensure_backup_quiescent()?;
        let (manifest, mut snapshot) = match &self.codec {
            Some(codec) => codec.decode(bytes,&state.modules)?,
            None => decode(bytes,&state.modules)?,
        };
        // References are not portable authentication authority on a new installation.
        snapshot.clear_credentials();
        // Reject newer writes even if they are only settings/credentials, not profiles.
        if self.library.snapshot()? != LibrarySnapshot::default() { return Err(LibraryError::LibraryNotEmpty.into()); }
        if let Some(owners)=&self.owners {owners.ensure_empty()?;}
        let current=self.snapshot()?;
        if current.owners.keys().ne(snapshot.owners.keys()) {return Err(BackupError::Module("portable owner scope does not match target adapters".into()));}
        for (id,owner) in &snapshot.owners {
            let available=&current.owners[id];
            if owner.schema_version!=available.schema_version || owner.credential_policy!=available.credential_policy || owner.excluded_fields!=available.excluded_fields {
                return Err(BackupError::Module(format!("portable owner contract mismatch: {id}")));
            }
        }
        if let Some(owners)=&self.owners {owners.validate(&snapshot.owners)?;}
        let import_digest = self.evidence_digest(&snapshot,true)?;
        let operation_id = operation_id.map(str::to_owned).unwrap_or_else(||Uuid::new_v4().to_string());
        gate.mark(&MaintenanceMarker { format_version: 1, owner_id: "mintcat.backup.portable".into(), operation_id: Some(operation_id.clone()), phase: MaintenancePhase::ImportingEmptyOwner, snapshot_digest: import_digest.clone() })?;
        // Failed commit stays durably blocked for explicit inspection, including ambiguous I/O errors.
        self.library.import_empty_snapshot(&snapshot.library)?;
        if let Some(owners)=&self.owners {owners.import_empty(&snapshot.owners)?;}
        gate.mark(&MaintenanceMarker { format_version: 1, owner_id: "mintcat.backup.portable".into(), operation_id: Some(operation_id), phase: MaintenancePhase::RestoreCommitted, snapshot_digest: import_digest })?;
        gate.clear()?;
        Ok(BackupResult { manifest, counts: snapshot.counts() })
    }
    /// Explicit recovery: only clear if the owner is exactly empty before commit, or exactly
    /// the imported snapshot after commit. Unknown/changed state remains blocked.
    pub fn recover_maintenance(&self, inspect: impl FnOnce() -> Result<BackupRuntimeState, BackupError>) -> Result<bool, BackupError> {
        let gate = MaintenanceGate::exclusive(&self.layout, true)?;
        let Some(marker) = gate.read()? else { return Ok(false); };
        if marker.format_version != 1 || !["mintcat.library","mintcat.backup.portable"].contains(&marker.owner_id.as_str()) || !matches!(marker.phase, MaintenancePhase::ImportingEmptyOwner | MaintenancePhase::RestoreCommitted) {
            return Err(BackupError::RecoveryRequired);
        }
        let state = inspect()?;
        state.blockers.ensure_quiescent(false)?;
        let snapshot = self.snapshot()?;
        let current_digest=if marker.owner_id=="mintcat.library" {digest(&serde_json::to_vec(&snapshot.library)?)} else {self.evidence_digest(&snapshot,false)?};
        let matches = current_digest == marker.snapshot_digest;
        let empty_before_commit = marker.phase == MaintenancePhase::ImportingEmptyOwner && snapshot.library == LibrarySnapshot::default()
            && self.owners.as_ref().map(|owners|owners.ensure_empty().is_ok()).unwrap_or(true);
        if !matches && !empty_before_commit { return Err(BackupError::RecoveryRequired); }
        gate.clear()?;
        Ok(true)
    }
    fn evidence_digest(&self,snapshot:&PortableSnapshot,imported:bool)->Result<String,BackupError> {
        if self.codec.is_none() {return Ok(digest(&serde_json::to_vec(snapshot)?));}
        let mut epochs=match &self.owners {Some(owners)=>owners.write_epochs()?,None=>BTreeMap::new()};
        epochs.insert("mintcat.library".into(),self.library.write_epoch()?);
        if imported {
            for (id,epoch) in &mut epochs {
                let changes=id!="mintcat.feature.proxy" || snapshot.owners.get(id).is_some_and(|owner|owner.content.as_object().is_some_and(|values|!values.is_empty()));
                if changes {*epoch=epoch.checked_add(1).ok_or_else(||BackupError::Invalid("owner epoch exhausted".into()))?;}
            }
        }
        Ok(digest(&serde_json::to_vec(&serde_json::json!({"snapshot":snapshot,"ownerEpochs":epochs}))?))
    }
    pub fn validate_bytes(bytes: &[u8], modules: &BTreeMap<String, ModuleVersion>) -> Result<BackupResult, BackupError> {
        let (manifest, snapshot) = decode(bytes, modules)?;
        Ok(BackupResult { manifest, counts: snapshot.counts() })
    }
}
fn encode_archive(snapshot: &PortableSnapshot, modules: BTreeMap<String, ModuleVersion>) -> Result<(Vec<u8>, BackupResult), BackupError> {
    validate_portable_bundle(snapshot, &modules)?;
    validate_modules(&modules)?;
    let content = serde_json::to_string(snapshot)?;
    let archive = BackupArchive { manifest: BackupManifest {
        format_version: BACKUP_FORMAT_VERSION, owner_id: "mintcat.backup.portable".into(), owner_schema_version: BACKUP_FORMAT_VERSION,
        module_versions: modules, credential_policy: "references-only-reauthenticate-on-import".into(),
        content_digest: digest(content.as_bytes()), content_size: content.len() as u64,
        consistency:"per-owner-logical-snapshots-under-common-maintenance-admission".into(),
        owner_schemas:std::iter::once(("mintcat.library".into(),LIBRARY_SCHEMA_VERSION)).chain(snapshot.owners.iter().map(|(id,owner)|(id.clone(),owner.schema_version))).collect(),
        excluded_authority:vec!["raw-secrets-and-vault-authority".into(),"kernel-jobs-actions-approvals".into(),"install-receipts-and-file-journals".into(),"proxy-ca-private-key-hosts-ownership-controller-and-service-leases".into()],
    }, content };
    let bytes = serde_json::to_vec(&archive)?;
    Ok((bytes, BackupResult { manifest: archive.manifest, counts: snapshot.counts() }))
}
fn decode(bytes: &[u8], modules: &BTreeMap<String, ModuleVersion>) -> Result<(BackupManifest, PortableSnapshot), BackupError> {
    let archive: BackupArchive = serde_json::from_slice(bytes)?;
    let manifest = archive.manifest;
    if manifest.format_version != BACKUP_FORMAT_VERSION || manifest.owner_id != "mintcat.backup.portable" || manifest.owner_schema_version != BACKUP_FORMAT_VERSION || manifest.credential_policy != "references-only-reauthenticate-on-import" {
        return Err(BackupError::Invalid("unsupported owner schema, format or credential policy".into()));
    }
    if manifest.content_size != archive.content.len() as u64 || manifest.content_digest != digest(archive.content.as_bytes()) {
        return Err(BackupError::Invalid("content digest/size mismatch".into()));
    }
    for (id, required) in &manifest.module_versions {
        if modules.get(id) != Some(required) { return Err(BackupError::Module(id.clone())); }
    }
    let snapshot: PortableSnapshot = serde_json::from_str(&archive.content)?;
    validate_portable_bundle(&snapshot, &manifest.module_versions)?;
    let owner_schemas:BTreeMap<_,_>=std::iter::once(("mintcat.library".into(),LIBRARY_SCHEMA_VERSION)).chain(snapshot.owners.iter().map(|(id,owner)|(id.clone(),owner.schema_version))).collect();
    if manifest.owner_schemas!=owner_schemas || manifest.consistency!="per-owner-logical-snapshots-under-common-maintenance-admission" {return Err(BackupError::Invalid("owner scope or consistency mismatch".into()));}
    validate_modules(&manifest.module_versions)?;
    Ok((manifest, snapshot))
}
fn validate_modules(modules: &BTreeMap<String, ModuleVersion>) -> Result<(), BackupError> {
    let library = modules.get("mintcat.library").ok_or_else(|| BackupError::Module("mintcat.library".into()))?;
    if library.schema_version != LIBRARY_SCHEMA_VERSION || library.version.is_empty() { return Err(BackupError::Module("mintcat.library".into())); }
    if modules.values().any(|module| module.version.is_empty() || module.schema_version == 0) { return Err(BackupError::Invalid("invalid module metadata".into())); }
    Ok(())
}
fn validate_portable_bundle(snapshot:&PortableSnapshot, modules:&BTreeMap<String,ModuleVersion>)->Result<(),BackupError> {
    validate_portable_snapshot(&snapshot.library)?;
    for (id,owner) in &snapshot.owners {
        let module=modules.get(id).ok_or_else(||BackupError::Module(id.clone()))?;
        if module.schema_version!=owner.schema_version || owner.credential_policy!="excluded-reauthenticate-no-live-authority" {
            return Err(BackupError::Module(id.clone()));
        }
        check_json(&owner.content)?;
    }
    Ok(())
}
fn sensitive(name: &str) -> bool {
    let normalized: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect();
    ["token", "secret", "password", "privatekey", "apikey", "authorization", "cookie"].iter().any(|word| normalized.contains(word))
}
fn check_json(value: &serde_json::Value) -> Result<(), BackupError> {
    match value {
        serde_json::Value::Object(values) => for (key, value) in values { if sensitive(key) && !value.is_null() { return Err(BackupError::Invalid("secret-bearing field is not portable".into())); } check_json(value)?; },
        serde_json::Value::Array(values) => for value in values { check_json(value)?; },
        serde_json::Value::String(text) => {
            if text.contains("-----BEGIN ") || text.starts_with("Bearer ") || (text.starts_with("eyJ") && text.split('.').count() == 3) { return Err(BackupError::Invalid("inline credential material is not portable".into())); }
            if let Ok(nested) = serde_json::from_str::<serde_json::Value>(text) { if !nested.is_string() { check_json(&nested)?; } }
            if let Ok(url) = url::Url::parse(text) { if !url.username().is_empty() || url.password().is_some() || url.query_pairs().any(|(key, _)| sensitive(&key)) { return Err(BackupError::Invalid("credential-bearing URL is not portable".into())); } }
        },
        _ => {},
    }
    Ok(())
}
fn validate_portable_snapshot(snapshot: &LibrarySnapshot) -> Result<(), BackupError> {
    for credential in &snapshot.credentials {
        if let Some(reference) = &credential.credential_ref {
            let id = reference.strip_prefix("credential:").and_then(|id| Uuid::parse_str(id).ok());
            if id.is_none() { return Err(BackupError::Invalid("credential value must be an opaque vault reference".into())); }
        }
    }
    for setting in &snapshot.settings { if sensitive(&setting.name) { return Err(BackupError::Invalid("secret-bearing setting is not portable".into())); } }
    if snapshot.download_operations.iter().any(|operation| !["succeeded", "cancelled", "failed"].contains(&operation.state.as_str())) || snapshot.operation_leases.iter().any(|lease| ["active", "suspect"].contains(&lease.state.as_str())) { return Err(BackupError::Busy("snapshot has unfinished downloads".into())); }
    if !snapshot.outbox.is_empty() || !snapshot.mutation_results.is_empty() || !snapshot.operation_leases.is_empty()
        || snapshot.download_operations.iter().any(|operation| operation.owner_scope!="portable-history" || operation.idempotency_scope!="portable-history") {
        return Err(BackupError::Invalid("execution or idempotency authority is not portable".into()));
    }
    check_json(&serde_json::to_value(snapshot)?)
}
pub(crate) fn digest(bytes: &[u8]) -> String { format!("sha256:{:x}", Sha256::digest(bytes)) }
pub(crate) fn sync_directory(path: &Path) -> Result<(), BackupError> {
    #[cfg(unix)] File::open(path)?.sync_all()?;
    Ok(())
}
pub(crate) fn atomic_write(target: &Path, bytes: &[u8], replace: bool) -> Result<(), BackupError> {
    let parent = target.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".backup-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if replace { fs::rename(&temporary, target)?; } else { fs::hard_link(&temporary, target)?; }
        sync_directory(parent)
    })();
    let _ = fs::remove_file(&temporary);
    result
}

#[cfg(test)] mod tests;
