mod archive;
mod settings;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Cursor, Read, Seek, SeekFrom},
};

use archive::{checked_component, checked_relative_path, scan_zip, ZipEntry};
use mintcat_runtime_artifacts::{ArtifactError, ArtifactGrant, ArtifactStore, ModuleLease};
use mintcat_sdk::{
    read_directory_snapshot_index, ArtifactRef, DirectorySnapshotError, DirectorySnapshotLimits,
};
use mintcat_unreal_pak::PakAlgorithmError;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use sha2::Digest as _;
use zip::ZipArchive;

pub use settings::Ue4ssSettings;

const UE4SS_SETTINGS_TARGET: &str = "ue4ss/ue4ss-settings.ini";
const PAK_MEDIA_TYPE: &str = "application/vnd.unreal.pak";
const PE_MEDIA_TYPE: &str = "application/vnd.microsoft.portable-executable";
const JAVASCRIPT_MEDIA_TYPE: &str = "text/javascript";

#[derive(Debug, Error)]
pub enum LoaderError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Pak(#[from] PakAlgorithmError),
    #[error("loader I/O failed while {scope}: {source}")]
    Io {
        scope: String,
        #[source]
        source: io::Error,
    },
    #[error("invalid loader request: {0}")]
    InvalidRequest(String),
    #[error("invalid loader input {input_id}: {reason}")]
    InvalidInput { input_id: String, reason: String },
    #[error("loader input limit exceeded for {input_id}: {limit}")]
    LimitExceeded { input_id: String, limit: String },
    #[error("loader archive error for {input_id}: {reason}")]
    Archive { input_id: String, reason: String },
    #[error("deployment target collision: {0}")]
    TargetCollision(String),
    #[error("resource package verification failed: {0}")]
    ResourceVerification(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoaderLimits {
    pub max_input_bytes: u64,
    pub max_entries: u64,
    pub max_entry_bytes: u64,
    pub max_expanded_bytes: u64,
    pub max_path_bytes: usize,
    pub max_compression_ratio: u64,
}

impl Default for LoaderLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 4 * 1024 * 1024 * 1024,
            max_entries: 20_000,
            max_entry_bytes: 2 * 1024 * 1024 * 1024,
            max_expanded_bytes: 8 * 1024 * 1024 * 1024,
            max_path_bytes: 4 * 1024,
            max_compression_ratio: 1_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeploymentContentKind {
    SignedResource,
    GeneratedSettings,
    Script,
    NativeDll,
    AudioPak,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum DeploymentRisk {
    ExecutableResource,
    ScriptExecution,
    NativeCodeExecution,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLock {
    pub resource_id: String,
    pub version: String,
    pub package_digest: String,
    pub publisher: String,
    pub signing_key_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalRequirement {
    pub risk: DeploymentRisk,
    pub target_relative_path: String,
    pub artifact_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_resource: Option<ResourceLock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentFile {
    pub target_relative_path: String,
    pub artifact: ArtifactRef,
    pub owner: String,
    pub content_kind: DeploymentContentKind,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeploymentManifest {
    pub schema_version: u32,
    pub game_id: String,
    pub resource_locks: Vec<ResourceLock>,
    pub files: Vec<DeploymentFile>,
    pub approval_requirements: Vec<ApprovalRequirement>,
    pub ignored_inputs: Vec<String>,
}

impl DeploymentManifest {
    pub fn merge(mut self, mut other: Self) -> Result<Self, LoaderError> {
        if self.game_id != other.game_id {
            return Err(LoaderError::InvalidRequest(format!(
                "cannot merge deployment manifests for {} and {}",
                self.game_id, other.game_id
            )));
        }
        self.resource_locks.append(&mut other.resource_locks);
        self.files.append(&mut other.files);
        self.approval_requirements
            .append(&mut other.approval_requirements);
        self.ignored_inputs.append(&mut other.ignored_inputs);
        finish_manifest(self)
    }

    pub fn owned_targets(&self) -> impl Iterator<Item = (&str, &str)> {
        self.files
            .iter()
            .map(|file| (file.target_relative_path.as_str(), file.owner.as_str()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceFileBinding {
    pub package_path: String,
    pub target_relative_path: String,
    pub media_type: Option<String>,
}

pub struct ResourceDeploymentRequest<'a> {
    pub game_id: String,
    pub owner: String,
    pub package: &'a ModuleLease,
    pub files: Vec<ResourceFileBinding>,
    pub settings: Ue4ssSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BundledResource {
    resource_id: String,
    version: String,
    #[serde(default)] archive_path: Option<String>,
    #[serde(default)] deployment_bindings: Vec<ResourceFileBinding>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum JsInputFormat {
    Zip,
    DirectorySnapshot,
}

pub struct JsDeploymentRequest<'a> {
    pub game_id: String,
    pub owner: String,
    pub mod_id: String,
    pub input_id: String,
    pub artifact: &'a ArtifactGrant,
    pub format: JsInputFormat,
}

pub struct NativeDeploymentRequest<'a> {
    pub game_id: String,
    pub owner: String,
    pub mod_id: String,
    pub input_id: String,
    pub artifact: &'a ArtifactGrant,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AudioInputFormat {
    Pak,
    Zip,
}

pub struct AudioDeploymentRequest<'a> {
    pub game_id: String,
    pub owner: String,
    pub source_id: String,
    pub mod_id: String,
    pub input_id: String,
    pub artifact: &'a ArtifactGrant,
    pub format: AudioInputFormat,
    pub paks_directory: String,
}

#[derive(Clone)]
pub struct Ue4ssLoader {
    artifacts: ArtifactStore,
    limits: LoaderLimits,
}

impl Ue4ssLoader {
    pub fn new(artifacts: ArtifactStore, limits: LoaderLimits) -> Result<Self, LoaderError> {
        validate_limits(limits)?;
        Ok(Self { artifacts, limits })
    }

    pub fn plan_resource_package(
        &self,
        request: ResourceDeploymentRequest<'_>,
    ) -> Result<DeploymentManifest, LoaderError> {
        validate_identity(&request.game_id, "game_id")?;
        validate_identity(&request.owner, "owner")?;
        if request.files.is_empty() {
            return Err(LoaderError::InvalidRequest(
                "resource package must bind at least one signed file".to_owned(),
            ));
        }
        let package = request.package.module();
        let bundled: Vec<BundledResource> = package.manifest.contributes.get("bundledResources")
            .cloned().map(serde_json::from_value).transpose()
            .map_err(|error| LoaderError::ResourceVerification(error.to_string()))?
            .unwrap_or_default();
        let mut resources = bundled.into_iter().filter(|resource| resource.resource_id == "resource.ue4ss");
        let bundled = resources.next();
        if resources.next().is_some() {
            return Err(LoaderError::ResourceVerification("duplicate bundled UE4SS resource".into()));
        }
        if let Some(resource) = &bundled {
            if resource.archive_path.is_some() || resource.deployment_bindings != request.files
                || package.manifest.contributes.get("scopeId").and_then(serde_json::Value::as_str) != Some(request.game_id.as_str()) {
                return Err(LoaderError::ResourceVerification("UE4SS deployment differs from the signed game resource bindings".into()));
            }
        } else if package.manifest.id != "resource.ue4ss" {
            return Err(LoaderError::ResourceVerification("package has no signed UE4SS resource identity".into()));
        }
        let lock = ResourceLock {
            resource_id: bundled.as_ref().map_or_else(|| package.manifest.id.clone(), |resource| resource.resource_id.clone()),
            version: bundled.as_ref().map_or_else(|| package.manifest.version.clone(), |resource| resource.version.clone()),
            package_digest: package.digest.clone(),
            publisher: package.manifest.publisher.clone(),
            signing_key_id: package.signature.key_id.clone(),
        };
        let mut signed_files = BTreeMap::new();
        for file in &package.manifest.files {
            if bundled.as_ref().is_some_and(|resource| !resource.deployment_bindings.iter().any(|binding| binding.package_path == file.path)) { continue; }
            let package_path = checked_relative_path(&file.path, self.limits.max_path_bytes)
                .map_err(LoaderError::ResourceVerification)?;
            let key = package_path.to_lowercase();
            if signed_files.insert(key, file).is_some() {
                return Err(LoaderError::ResourceVerification(format!(
                    "signed package contains case-colliding path {}",
                    file.path
                )));
            }
        }
        let mut bound_sources = BTreeSet::new();
        let mut files = Vec::with_capacity(request.files.len());
        let mut approvals = Vec::new();
        let mut patched_settings = false;
        for binding in request.files {
            let package_path =
                checked_relative_path(&binding.package_path, self.limits.max_path_bytes)
                    .map_err(LoaderError::ResourceVerification)?;
            let target = checked_deployment_target(&binding.target_relative_path, self.limits)?;
            let source_key = package_path.to_lowercase();
            if !bound_sources.insert(source_key.clone()) {
                return Err(LoaderError::ResourceVerification(format!(
                    "resource file {} is bound more than once",
                    binding.package_path
                )));
            }
            let signed = signed_files.get(&source_key).ok_or_else(|| {
                LoaderError::ResourceVerification(format!(
                    "resource file {} is absent from the signed manifest",
                    binding.package_path
                ))
            })?;
            if signed.path != package_path {
                return Err(LoaderError::ResourceVerification(format!(
                    "resource path case differs from signed path {}",
                    signed.path
                )));
            }
            if is_executable_target(&target) && !signed.executable {
                return Err(LoaderError::ResourceVerification(format!(
                    "executable resource {target} is not marked executable by its signed manifest"
                )));
            }
            let source = package.entry_path(&signed.path)?;
            let is_settings = target.eq_ignore_ascii_case(UE4SS_SETTINGS_TARGET);
            let (artifact, content_kind) = if is_settings && !request.settings.is_empty() {
                let metadata = source.metadata().map_err(|source| LoaderError::Io {
                    scope: "reading signed UE4SS settings metadata".to_owned(),
                    source,
                })?;
                if metadata.len() > self.limits.max_entry_bytes {
                    return Err(LoaderError::LimitExceeded {
                        input_id: signed.path.clone(),
                        limit: format!(
                            "settings resource exceeds {} bytes",
                            self.limits.max_entry_bytes
                        ),
                    });
                }
                let original = std::fs::read(&source).map_err(|source| LoaderError::Io {
                    scope: "reading signed UE4SS settings".to_owned(),
                    source,
                })?;
                if format!("sha256:{:x}", sha2::Sha256::digest(&original)) != signed.sha256 || original.len() as u64 != signed.size {
                    return Err(LoaderError::ResourceVerification(format!("signed resource {} changed after package verification", signed.path)));
                }
                let configured = request.settings.apply(&original)?;
                let mut reader = Cursor::new(configured);
                patched_settings = true;
                (
                    self.artifacts.publish_blob_reader(
                        &mut reader,
                        binding
                            .media_type
                            .or_else(|| Some("text/plain; charset=utf-8".to_owned())),
                    )?,
                    DeploymentContentKind::GeneratedSettings,
                )
            } else {
                let artifact = self.artifacts.publish_blob_file(
                    &source,
                    binding.media_type.or_else(|| infer_media_type(&target)),
                )?;
                if artifact.digest != signed.sha256 || artifact.size != signed.size {
                    return Err(LoaderError::ResourceVerification(format!(
                        "signed resource {} changed after package verification",
                        signed.path
                    )));
                }
                (artifact, DeploymentContentKind::SignedResource)
            };
            if signed.executable {
                approvals.push(ApprovalRequirement {
                    risk: DeploymentRisk::ExecutableResource,
                    target_relative_path: target.clone(),
                    artifact_digest: artifact.digest.clone(),
                    signed_resource: Some(lock.clone()),
                });
            }
            files.push(DeploymentFile {
                target_relative_path: target,
                artifact,
                owner: request.owner.clone(),
                content_kind,
                executable: signed.executable,
            });
        }
        if bound_sources.len() != signed_files.len() {
            let unbound = signed_files
                .iter()
                .find(|(key, _)| !bound_sources.contains(*key))
                .map(|(_, file)| file.path.as_str())
                .unwrap_or("unknown");
            return Err(LoaderError::ResourceVerification(format!(
                "signed package file {unbound} has no explicit deployment binding"
            )));
        }
        if !request.settings.is_empty() && !patched_settings {
            return Err(LoaderError::ResourceVerification(
                "game-specific UE4SS settings require a ue4ss/UE4SS-settings.ini binding"
                    .to_owned(),
            ));
        }
        finish_manifest(DeploymentManifest {
            schema_version: 1,
            game_id: request.game_id,
            resource_locks: vec![lock],
            files,
            approval_requirements: approvals,
            ignored_inputs: Vec::new(),
        })
    }

    /// A PAK-bearing ZIP may also contain the legacy root main.dll or a JS mod tree.
    /// Inspect the archive before creating any executable or script deployment entries.
    pub fn plan_pak_zip_companions(
        &self,
        request: JsDeploymentRequest<'_>,
    ) -> Result<Vec<DeploymentManifest>, LoaderError> {
        let input_id = &request.input_id;
        let (file, _) = open_grant(request.artifact, input_id, self.limits.max_input_bytes, None)?;
        let archive_error = |error: zip::result::ZipError| LoaderError::Archive {
            input_id: input_id.clone(),
            reason: error.to_string(),
        };
        let mut archive = ZipArchive::new(file).map_err(archive_error)?;
        let entries = scan_zip(&mut archive, input_id, self.limits)?;
        let mut manifests = Vec::new();
        if let Some(dll) = entries.iter().find(|entry| !entry.directory && entry.path.eq_ignore_ascii_case("main.dll")) {
            let entry = archive.by_index(dll.index).map_err(archive_error)?;
            let artifact = self.artifacts.publish_blob_reader(
                &mut entry.take(dll.size.saturating_add(1)), Some(PE_MEDIA_TYPE.to_owned()),
            )?;
            if artifact.size != dll.size {
                return Err(LoaderError::Archive {
                    input_id: input_id.clone(),
                    reason: "native entry does not match its declared size".to_owned(),
                });
            }
            let grant = self.artifacts.grant_blob(&artifact)?;
            manifests.push(self.plan_native_mod(NativeDeploymentRequest {
                game_id: request.game_id.clone(),
                owner: request.owner.clone(),
                mod_id: request.mod_id.clone(),
                input_id: input_id.clone(),
                artifact: &grant,
            })?);
        }
        if entries.iter().any(|entry| !entry.directory
            && (entry.path == "js/main.js" || entry.path.ends_with("/js/main.js")))
        {
            validate_identity(&request.game_id, "game_id")?;
            validate_identity(&request.owner, "owner")?;
            let mod_id = checked_component(&request.mod_id).map_err(LoaderError::InvalidRequest)?;
            let published = self.publish_js_zip_entries(input_id, &mut archive, &entries)?;
            manifests.push(self.plan_published_js(request, &mod_id, published)?);
        }
        Ok(manifests)
    }

    pub fn plan_js_mod(
        &self,
        request: JsDeploymentRequest<'_>,
    ) -> Result<DeploymentManifest, LoaderError> {
        validate_identity(&request.game_id, "game_id")?;
        validate_identity(&request.owner, "owner")?;
        let mod_id = checked_component(&request.mod_id).map_err(LoaderError::InvalidRequest)?;
        let (mut file, _) = open_grant(
            request.artifact,
            &request.input_id,
            self.limits.max_input_bytes,
            None,
        )?;
        let published = match request.format {
            JsInputFormat::Zip => self.publish_js_zip(&request.input_id, file)?,
            JsInputFormat::DirectorySnapshot => {
                self.publish_js_snapshot(&request.input_id, &mut file)?
            }
        };
        self.plan_published_js(request, &mod_id, published)
    }

    fn plan_published_js(
        &self,
        request: JsDeploymentRequest<'_>,
        mod_id: &str,
        published: PublishedJs,
    ) -> Result<DeploymentManifest, LoaderError> {
        let PublishedJs { artifacts, ignored } = published;
        let mut files = Vec::with_capacity(artifacts.len());
        let mut approvals = Vec::with_capacity(artifacts.len());
        for (tail, artifact) in artifacts {
            let target =
                checked_deployment_target(&format!("ue4ss/mods/{mod_id}/js/{tail}"), self.limits)?;
            approvals.push(ApprovalRequirement {
                risk: DeploymentRisk::ScriptExecution,
                target_relative_path: target.clone(),
                artifact_digest: artifact.digest.clone(),
                signed_resource: None,
            });
            files.push(DeploymentFile {
                target_relative_path: target,
                artifact,
                owner: request.owner.clone(),
                content_kind: DeploymentContentKind::Script,
                executable: false,
            });
        }
        finish_manifest(DeploymentManifest {
            schema_version: 1,
            game_id: request.game_id,
            resource_locks: Vec::new(),
            files,
            approval_requirements: approvals,
            ignored_inputs: ignored,
        })
    }

    pub fn plan_native_mod(
        &self,
        request: NativeDeploymentRequest<'_>,
    ) -> Result<DeploymentManifest, LoaderError> {
        validate_identity(&request.game_id, "game_id")?;
        validate_identity(&request.owner, "owner")?;
        let mod_id = checked_component(&request.mod_id).map_err(LoaderError::InvalidRequest)?;
        let (mut file, artifact) = open_grant(
            request.artifact,
            &request.input_id,
            self.limits.max_entry_bytes,
            Some(PE_MEDIA_TYPE.to_owned()),
        )?;
        archive::validate_pe_dll(&mut file, &request.input_id, self.limits.max_entry_bytes)?;
        let target =
            checked_deployment_target(&format!("ue4ss/mods/{mod_id}/main.dll"), self.limits)?;
        finish_manifest(DeploymentManifest {
            schema_version: 1,
            game_id: request.game_id,
            resource_locks: Vec::new(),
            files: vec![DeploymentFile {
                target_relative_path: target.clone(),
                artifact: artifact.clone(),
                owner: request.owner,
                content_kind: DeploymentContentKind::NativeDll,
                executable: true,
            }],
            approval_requirements: vec![ApprovalRequirement {
                risk: DeploymentRisk::NativeCodeExecution,
                target_relative_path: target,
                artifact_digest: artifact.digest,
                signed_resource: None,
            }],
            ignored_inputs: Vec::new(),
        })
    }

    pub fn plan_audio_pak(
        &self,
        request: AudioDeploymentRequest<'_>,
    ) -> Result<DeploymentManifest, LoaderError> {
        validate_identity(&request.game_id, "game_id")?;
        validate_identity(&request.owner, "owner")?;
        let source_id =
            checked_component(&request.source_id).map_err(LoaderError::InvalidRequest)?;
        let mod_id = checked_component(&request.mod_id).map_err(LoaderError::InvalidRequest)?;
        let paks_directory =
            checked_relative_path(&request.paks_directory, self.limits.max_path_bytes)
                .map_err(LoaderError::InvalidRequest)?;
        let (mut file, raw_artifact) = open_grant(
            request.artifact,
            &request.input_id,
            self.limits.max_input_bytes,
            Some(PAK_MEDIA_TYPE.to_owned()),
        )?;
        let artifact = match request.format {
            AudioInputFormat::Pak => {
                archive::validate_audio_pak(&mut file, &request.input_id, self.limits)?;
                raw_artifact
            }
            AudioInputFormat::Zip => {
                let mut zip = ZipArchive::new(file).map_err(|error| LoaderError::Archive {
                    input_id: request.input_id.clone(),
                    reason: error.to_string(),
                })?;
                let entries = scan_zip(&mut zip, &request.input_id, self.limits)?;
                let payloads = entries
                    .iter()
                    .filter(|entry| !entry.directory)
                    .collect::<Vec<_>>();
                if payloads.len() != 1 || !payloads[0].path.to_ascii_lowercase().ends_with(".pak") {
                    return Err(LoaderError::InvalidInput {
                        input_id: request.input_id.clone(),
                        reason: "audio ZIP must contain exactly one PAK file".to_owned(),
                    });
                }
                let payload = payloads[0];
                let mut entry =
                    zip.by_index(payload.index)
                        .map_err(|error| LoaderError::Archive {
                            input_id: request.input_id.clone(),
                            reason: error.to_string(),
                        })?;
                let artifact = self
                    .artifacts
                    .publish_blob_reader(&mut entry, Some(PAK_MEDIA_TYPE.to_owned()))?;
                if artifact.size != payload.size {
                    return Err(LoaderError::Archive {
                        input_id: request.input_id.clone(),
                        reason: "audio PAK ended before its declared ZIP size".to_owned(),
                    });
                }
                let grant = self.artifacts.grant_blob(&artifact)?;
                let mut published = grant.open_read()?;
                archive::validate_audio_pak(&mut published, &request.input_id, self.limits)?;
                artifact
            }
        };
        let target = checked_deployment_target(
            &format!("{paks_directory}/{source_id}-{mod_id}_P.pak"),
            self.limits,
        )?;
        finish_manifest(DeploymentManifest {
            schema_version: 1,
            game_id: request.game_id,
            resource_locks: Vec::new(),
            files: vec![DeploymentFile {
                target_relative_path: target,
                artifact,
                owner: request.owner,
                content_kind: DeploymentContentKind::AudioPak,
                executable: false,
            }],
            approval_requirements: Vec::new(),
            ignored_inputs: Vec::new(),
        })
    }

    fn publish_js_zip(&self, input_id: &str, file: File) -> Result<PublishedJs, LoaderError> {
        let mut archive = ZipArchive::new(file).map_err(|error| LoaderError::Archive {
            input_id: input_id.to_owned(),
            reason: error.to_string(),
        })?;
        let entries = scan_zip(&mut archive, input_id, self.limits)?;
        self.publish_js_zip_entries(input_id, &mut archive, &entries)
    }

    fn publish_js_zip_entries(
        &self,
        input_id: &str,
        archive: &mut ZipArchive<File>,
        entries: &[ZipEntry],
    ) -> Result<PublishedJs, LoaderError> {
        let records = entries
            .iter()
            .filter(|entry| !entry.directory)
            .map(|entry| PathRecord {
                index: entry.index,
                path: entry.path.clone(),
                size: entry.size,
            })
            .collect::<Vec<_>>();
        let (selected, ignored) = select_js_files(input_id, records)?;
        let mut artifacts = Vec::with_capacity(selected.len());
        for selected in selected {
            let entry =
                archive
                    .by_index(selected.index)
                    .map_err(|error| LoaderError::Archive {
                        input_id: input_id.to_owned(),
                        reason: error.to_string(),
                    })?;
            let artifact = self
                .artifacts
                .publish_blob_reader(&mut entry.take(selected.size.saturating_add(1)), infer_js_media_type(&selected.tail))?;
            if artifact.size != selected.size {
                return Err(LoaderError::Archive {
                    input_id: input_id.to_owned(),
                    reason: format!("JS entry {} does not match its declared size", selected.path),
                });
            }
            artifacts.push((selected.tail, artifact));
        }
        Ok(PublishedJs { artifacts, ignored })
    }

    fn publish_js_snapshot(
        &self,
        input_id: &str,
        file: &mut File,
    ) -> Result<PublishedJs, LoaderError> {
        let snapshot = read_directory_snapshot_index(
            file,
            DirectorySnapshotLimits {
                max_entries: self.limits.max_entries,
                max_path_bytes: self.limits.max_path_bytes,
                max_entry_bytes: self.limits.max_entry_bytes,
                max_total_bytes: self.limits.max_expanded_bytes,
            },
        )
        .map_err(|error| map_snapshot_error(input_id, error))?;
        let records = snapshot
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| PathRecord {
                index,
                path: entry.relative_path.clone(),
                size: entry.size,
            })
            .collect::<Vec<_>>();
        let (selected, ignored) = select_js_files(input_id, records)?;
        let mut artifacts = Vec::with_capacity(selected.len());
        for selected in selected {
            let entry = &snapshot.entries[selected.index];
            file.seek(SeekFrom::Start(entry.content_offset))
                .map_err(|source| LoaderError::Io {
                    scope: "seeking JS directory snapshot entry".to_owned(),
                    source,
                })?;
            let mut payload = Read::by_ref(file).take(entry.size);
            let artifact = self
                .artifacts
                .publish_blob_reader(&mut payload, infer_js_media_type(&selected.tail))?;
            if artifact.size != entry.size {
                return Err(LoaderError::Archive {
                    input_id: input_id.to_owned(),
                    reason: format!(
                        "JS snapshot entry {} ended before its declared size",
                        selected.path
                    ),
                });
            }
            artifacts.push((selected.tail, artifact));
        }
        Ok(PublishedJs { artifacts, ignored })
    }
}

#[derive(Debug)]
struct PublishedJs {
    artifacts: Vec<(String, ArtifactRef)>,
    ignored: Vec<String>,
}

#[derive(Debug, Clone)]
struct PathRecord {
    index: usize,
    path: String,
    size: u64,
}

#[derive(Debug, Clone)]
struct JsSelection {
    index: usize,
    path: String,
    tail: String,
    size: u64,
}

fn select_js_files(
    input_id: &str,
    records: Vec<PathRecord>,
) -> Result<(Vec<JsSelection>, Vec<String>), LoaderError> {
    let mut roots = records
        .iter()
        .filter_map(|record| {
            if record.path == "js/main.js" {
                Some(String::new())
            } else {
                record.path.strip_suffix("/js/main.js").map(str::to_owned)
            }
        })
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    if roots.len() != 1 {
        return Err(LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: format!(
                "JS input must contain exactly one js/main.js root, found {}",
                roots.len()
            ),
        });
    }
    let prefix = if roots[0].is_empty() {
        "js/".to_owned()
    } else {
        format!("{}/js/", roots[0])
    };
    let mut selected = Vec::new();
    let mut ignored = Vec::new();
    for record in records {
        if let Some(tail) = record.path.strip_prefix(&prefix).map(str::to_owned) {
            if tail.is_empty() {
                continue;
            }
            selected.push(JsSelection {
                index: record.index,
                path: record.path,
                tail,
                size: record.size,
            });
        } else {
            ignored.push(record.path);
        }
    }
    selected.sort_by(|left, right| {
        left.tail
            .to_lowercase()
            .cmp(&right.tail.to_lowercase())
            .then_with(|| left.tail.cmp(&right.tail))
    });
    ignored.sort();
    Ok((selected, ignored))
}

fn finish_manifest(mut manifest: DeploymentManifest) -> Result<DeploymentManifest, LoaderError> {
    validate_identity(&manifest.game_id, "game_id")?;
    if manifest.schema_version != 1 {
        return Err(LoaderError::InvalidRequest(
            "deployment manifest schema version must be 1".to_owned(),
        ));
    }
    let mut locks = BTreeMap::new();
    for lock in &manifest.resource_locks {
        validate_identity(&lock.resource_id, "resource id")?;
        validate_identity(&lock.version, "resource version")?;
        let key = lock.resource_id.to_lowercase();
        if let Some(existing) = locks.insert(key, lock.clone()) {
            if existing != *lock {
                return Err(LoaderError::TargetCollision(format!(
                    "resource {} is locked to multiple versions",
                    lock.resource_id
                )));
            }
        }
    }
    manifest.resource_locks = locks.into_values().collect();

    let mut targets = BTreeMap::new();
    for file in &manifest.files {
        validate_identity(&file.owner, "deployment owner")?;
        let target = checked_relative_path(&file.target_relative_path, 4 * 1024)
            .map_err(LoaderError::InvalidRequest)?;
        if target != file.target_relative_path {
            return Err(LoaderError::InvalidRequest(format!(
                "deployment target must be normalized: {}",
                file.target_relative_path
            )));
        }
        let key = target.to_lowercase();
        if targets.insert(key, file).is_some() {
            return Err(LoaderError::TargetCollision(target));
        }
        if !file.artifact.verified {
            return Err(LoaderError::InvalidRequest(format!(
                "deployment artifact {} is not verified",
                file.artifact.digest
            )));
        }
    }
    let mut approval_targets = BTreeSet::new();
    for approval in &manifest.approval_requirements {
        let key = approval.target_relative_path.to_lowercase();
        if !approval_targets.insert((key.clone(), approval.risk.clone())) {
            return Err(LoaderError::TargetCollision(format!(
                "duplicate approval for {}",
                approval.target_relative_path
            )));
        }
        let file = targets.get(&key).ok_or_else(|| {
            LoaderError::InvalidRequest(format!(
                "approval target {} is absent from deployment files",
                approval.target_relative_path
            ))
        })?;
        if file.target_relative_path != approval.target_relative_path {
            return Err(LoaderError::InvalidRequest(format!(
                "approval target case differs for {}",
                approval.target_relative_path
            )));
        }
        if file.artifact.digest != approval.artifact_digest {
            return Err(LoaderError::InvalidRequest(format!(
                "approval digest differs for {}",
                approval.target_relative_path
            )));
        }
        let valid_risk = match approval.risk {
            DeploymentRisk::ExecutableResource => {
                file.content_kind == DeploymentContentKind::SignedResource && file.executable
            }
            DeploymentRisk::ScriptExecution => {
                file.content_kind == DeploymentContentKind::Script && !file.executable
            }
            DeploymentRisk::NativeCodeExecution => {
                file.content_kind == DeploymentContentKind::NativeDll && file.executable
            }
        };
        if !valid_risk {
            return Err(LoaderError::InvalidRequest(format!(
                "approval risk does not match deployment content for {}",
                approval.target_relative_path
            )));
        }
        if approval.risk == DeploymentRisk::ExecutableResource {
            let lock = approval.signed_resource.as_ref().ok_or_else(|| {
                LoaderError::InvalidRequest(
                    "executable resource approval lacks a signed resource lock".to_owned(),
                )
            })?;
            if !manifest.resource_locks.contains(lock) {
                return Err(LoaderError::InvalidRequest(
                    "executable resource approval references an unlocked package".to_owned(),
                ));
            }
        } else if approval.signed_resource.is_some() {
            return Err(LoaderError::InvalidRequest(
                "only executable signed resources may carry a resource lock in approval".to_owned(),
            ));
        }
    }
    for (key, file) in &targets {
        let required_risk = match file.content_kind {
            DeploymentContentKind::SignedResource if file.executable => {
                Some(DeploymentRisk::ExecutableResource)
            }
            DeploymentContentKind::Script => Some(DeploymentRisk::ScriptExecution),
            DeploymentContentKind::NativeDll if file.executable => {
                Some(DeploymentRisk::NativeCodeExecution)
            }
            DeploymentContentKind::SignedResource
            | DeploymentContentKind::GeneratedSettings
            | DeploymentContentKind::AudioPak => None,
            DeploymentContentKind::NativeDll => {
                return Err(LoaderError::InvalidRequest(format!(
                    "native deployment {} is not executable",
                    file.target_relative_path
                )));
            }
        };
        if let Some(risk) = required_risk {
            if !approval_targets.contains(&(key.clone(), risk)) {
                return Err(LoaderError::InvalidRequest(format!(
                    "deployment {} lacks its required approval",
                    file.target_relative_path
                )));
            }
        } else if file.executable {
            return Err(LoaderError::InvalidRequest(format!(
                "non-code deployment {} is unexpectedly executable",
                file.target_relative_path
            )));
        }
    }
    manifest.files.sort_by(|left, right| {
        left.target_relative_path
            .to_lowercase()
            .cmp(&right.target_relative_path.to_lowercase())
            .then_with(|| left.target_relative_path.cmp(&right.target_relative_path))
    });
    manifest.approval_requirements.sort_by(|left, right| {
        left.target_relative_path
            .to_lowercase()
            .cmp(&right.target_relative_path.to_lowercase())
            .then_with(|| left.risk.cmp(&right.risk))
    });
    manifest.ignored_inputs.sort();
    manifest.ignored_inputs.dedup();
    Ok(manifest)
}

fn open_grant(
    grant: &ArtifactGrant,
    input_id: &str,
    max_bytes: u64,
    media_type: Option<String>,
) -> Result<(File, ArtifactRef), LoaderError> {
    if grant.descriptor.size == 0 || grant.descriptor.size > max_bytes {
        return Err(LoaderError::LimitExceeded {
            input_id: input_id.to_owned(),
            limit: format!("artifact must contain between 1 and {max_bytes} bytes"),
        });
    }
    let file = grant.open_read()?;
    let size = file
        .metadata()
        .map_err(|source| LoaderError::Io {
            scope: "reading granted artifact metadata".to_owned(),
            source,
        })?
        .len();
    if size != grant.descriptor.size {
        return Err(LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: format!(
                "artifact grant declares {} bytes but file contains {size}",
                grant.descriptor.size
            ),
        });
    }
    Ok((
        file,
        ArtifactRef {
            digest: grant.descriptor.digest.clone(),
            size,
            verified: true,
            media_type,
        },
    ))
}

fn checked_deployment_target(value: &str, limits: LoaderLimits) -> Result<String, LoaderError> {
    checked_relative_path(value, limits.max_path_bytes).map_err(LoaderError::InvalidRequest)
}

fn validate_identity(value: &str, field: &str) -> Result<(), LoaderError> {
    if value.trim().is_empty() || value.contains(['\0', '\n', '\r']) {
        Err(LoaderError::InvalidRequest(format!(
            "{field} must be non-empty and single-line"
        )))
    } else {
        Ok(())
    }
}

fn validate_limits(limits: LoaderLimits) -> Result<(), LoaderError> {
    if limits.max_input_bytes == 0
        || limits.max_entries == 0
        || limits.max_entry_bytes == 0
        || limits.max_expanded_bytes == 0
        || limits.max_path_bytes == 0
        || limits.max_compression_ratio == 0
    {
        return Err(LoaderError::InvalidRequest(
            "all loader limits must be positive".to_owned(),
        ));
    }
    Ok(())
}

fn is_executable_target(path: &str) -> bool {
    let lowercase = path.to_ascii_lowercase();
    lowercase.ends_with(".dll") || lowercase.ends_with(".exe")
}

fn infer_media_type(path: &str) -> Option<String> {
    let lowercase = path.to_ascii_lowercase();
    if lowercase.ends_with(".dll") || lowercase.ends_with(".exe") {
        Some(PE_MEDIA_TYPE.to_owned())
    } else if lowercase.ends_with(".ini") || lowercase.ends_with(".txt") {
        Some("text/plain; charset=utf-8".to_owned())
    } else {
        Some("application/octet-stream".to_owned())
    }
}

fn infer_js_media_type(path: &str) -> Option<String> {
    if path.to_ascii_lowercase().ends_with(".js") {
        Some(JAVASCRIPT_MEDIA_TYPE.to_owned())
    } else {
        Some("application/octet-stream".to_owned())
    }
}

fn map_snapshot_error(input_id: &str, error: DirectorySnapshotError) -> LoaderError {
    match error {
        DirectorySnapshotError::Limit(limit) => LoaderError::LimitExceeded {
            input_id: input_id.to_owned(),
            limit,
        },
        error => LoaderError::Archive {
            input_id: input_id.to_owned(),
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests;
