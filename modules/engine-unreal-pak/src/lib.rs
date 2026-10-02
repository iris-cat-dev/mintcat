mod input;
mod backend;
mod manifest;
pub mod worker_api;
pub mod game_inputs;

pub use backend::{RemoteUnrealPakEngine, UnrealEngineBackend};
pub use manifest::module_manifest;

pub const ENGINE_CONTRACT_ID: &str = "engine.unreal-pak";
pub const ENGINE_CONTRACT_VERSION: u64 = 1;
pub const ENGINE_MODULE_ID: &str = "mintcat.engine.unreal-pak";
pub const ENGINE_MODULE_VERSION: &str = env!("CARGO_PKG_VERSION");

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use memmap2::MmapOptions;
use mintcat_runtime_artifacts::{ArtifactError, ArtifactGrant, ArtifactStore};
use mintcat_sdk::ArtifactRef;
use mintcat_unreal_pak::{
    normalize_entry_path, validate_output_path, PakAlgorithmError, PakArchive, PakBuildTarget,
    PakBundleWriter, PakMetadata,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tempfile::Builder;
use thiserror::Error;

use input::MaterializedInputs;

const OUTPUT_MEDIA_TYPE: &str = "application/vnd.unreal.pak";

#[derive(Debug, Error)]
pub enum UnrealEngineError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Pak(#[from] PakAlgorithmError),
    #[error("Unreal engine I/O failed while {scope}: {source}")]
    Io {
        scope: String,
        #[source]
        source: io::Error,
    },
    #[error("invalid Unreal input {input_id}: {reason}")]
    InvalidInput { input_id: String, reason: String },
    #[error("duplicate Unreal input id: {0}")]
    DuplicateInput(String),
    #[error("Unreal input limit exceeded for {input_id}: {limit}")]
    LimitExceeded { input_id: String, limit: String },
    #[error("Unreal archive error for {input_id}: {reason}")]
    Archive { input_id: String, reason: String },
    #[error("Unreal build verification failed: {0}")]
    Verification(String),
    #[error("invalid Unreal build request: {0}")]
    InvalidRequest(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Worker(#[from] mintcat_sdk::WorkerProtocolError),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UnrealInputFormat {
    Pak,
    Zip,
    DirectorySnapshot,
}

pub struct UnrealInput<'a> {
    pub input_id: String,
    pub artifact: &'a ArtifactGrant,
    pub format: UnrealInputFormat,
    pub content_prefix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnrealEngineLimits {
    pub max_input_bytes: u64,
    pub max_entries: u64,
    pub max_entry_bytes: u64,
    pub max_expanded_bytes: u64,
    pub max_path_bytes: usize,
    pub max_compression_ratio: u64,
}

impl Default for UnrealEngineLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024 * 1024,
            max_entries: 100_000,
            max_entry_bytes: 8 * 1024 * 1024 * 1024,
            max_expanded_bytes: 64 * 1024 * 1024 * 1024,
            max_path_bytes: 4 * 1024,
            max_compression_ratio: 1_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PakContainerInspection {
    pub source_name: String,
    pub metadata: PakMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputInspection {
    pub input_id: String,
    pub format: UnrealInputFormat,
    pub files: Vec<String>,
    pub pak_containers: Vec<PakContainerInspection>,
    pub ignored_entries: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileConflict {
    pub relative_path: String,
    pub winner_input_id: String,
    pub shadowed_input_ids: Vec<String>,
}

pub struct UnrealBuildRequest<'a> {
    pub plan_hash: String,
    pub inputs: Vec<UnrealInput<'a>>,
    pub reference_paths: Vec<String>,
    pub target: PakBuildTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuiltFile {
    pub relative_path: String,
    pub digest: String,
    pub size: u64,
    pub winner_input_id: String,
    pub shadowed_input_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnrealBuildManifest {
    pub schema_version: u32,
    pub plan_hash: String,
    pub build_conditions_digest: String,
    pub target: PakBuildTarget,
    pub files: Vec<BuiltFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnrealBuildResult {
    pub artifact: ArtifactRef,
    pub manifest: UnrealBuildManifest,
    pub conflicts: Vec<FileConflict>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationReport {
    pub artifact_digest: String,
    pub file_count: u64,
    pub expanded_bytes: u64,
    pub metadata: PakMetadata,
}

#[derive(Clone)]
pub struct UnrealPakEngine {
    artifacts: ArtifactStore,
    staging: PathBuf,
    limits: UnrealEngineLimits,
}

impl UnrealPakEngine {
    pub fn open(
        artifacts: ArtifactStore,
        staging: impl Into<PathBuf>,
        limits: UnrealEngineLimits,
    ) -> Result<Self, UnrealEngineError> {
        validate_limits(limits)?;
        let staging = staging.into();
        fs::create_dir_all(&staging)
            .map_err(|source| io_error("creating engine staging", source))?;
        let metadata = fs::symlink_metadata(&staging)
            .map_err(|source| io_error("inspecting engine staging", source))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(UnrealEngineError::InvalidRequest(
                "engine staging must be a real directory".to_owned(),
            ));
        }
        Ok(Self {
            artifacts,
            staging,
            limits,
        })
    }

    pub fn inspect(&self, input: UnrealInput<'_>) -> Result<InputInspection, UnrealEngineError> {
        let workspace = self.workspace()?;
        let materialized = MaterializedInputs::open(vec![input], self.limits, workspace.path())?;
        Ok(materialized
            .inspections
            .into_iter()
            .next()
            .expect("one input"))
    }

    pub fn conflicts(
        &self,
        inputs: Vec<UnrealInput<'_>>,
    ) -> Result<Vec<FileConflict>, UnrealEngineError> {
        let workspace = self.workspace()?;
        let materialized = MaterializedInputs::open(inputs, self.limits, workspace.path())?;
        Ok(resolve_winners(&materialized).1)
    }

    pub fn build(
        &self,
        request: UnrealBuildRequest<'_>,
    ) -> Result<UnrealBuildResult, UnrealEngineError> {
        validate_build_request(&request)?;
        let conditions_digest = build_conditions_digest(&request)?;
        let workspace = self.workspace()?;
        let mut materialized =
            MaterializedInputs::open(request.inputs, self.limits, workspace.path())?;
        if let Some(inspection) = materialized.inspections.iter().find(|inspection| {
            inspection.pak_containers.iter().any(|container| {
                container
                    .metadata
                    .codecs
                    .contains(&mintcat_unreal_pak::PakCodec::Oodle)
            })
        }) {
            return Err(UnrealEngineError::InvalidInput {
                input_id: inspection.input_id.clone(),
                reason: "Oodle-compressed input is inspectable but this build module has no approved Oodle codec".to_owned(),
            });
        }
        let (winner_indices, conflicts) = resolve_winners(&materialized);

        let output_path = workspace.path().join("output.pak");
        let output = File::create(&output_path)
            .map_err(|source| io_error("creating build output", source))?;
        let mut writer = PakBundleWriter::new(output, &request.reference_paths, &request.target)?;
        let mut spool = (!winner_indices.is_empty())
            .then(|| tempfile::tempfile_in(workspace.path()))
            .transpose()
            .map_err(|source| io_error("creating entry spool", source))?;
        let conflict_by_path = conflicts
            .iter()
            .map(|conflict| {
                (
                    conflict.relative_path.to_ascii_lowercase(),
                    conflict.shadowed_input_ids.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut files = Vec::with_capacity(winner_indices.len());
        let mut total = 0_u64;

        for candidate_index in winner_indices {
            let spool = spool.as_mut().expect("nonempty builds have an entry spool");
            spool
                .set_len(0)
                .map_err(|source| io_error("resetting entry spool", source))?;
            spool
                .seek(SeekFrom::Start(0))
                .map_err(|source| io_error("rewinding entry spool", source))?;
            let mut digest_writer =
                LimitedDigestWriter::new(&mut *spool, self.limits.max_entry_bytes);
            let copy_result = materialized.copy_candidate(candidate_index, &mut digest_writer);
            if digest_writer.exceeded {
                let input_id = materialized.candidates[candidate_index].input_id.clone();
                return Err(UnrealEngineError::LimitExceeded {
                    input_id,
                    limit: format!("entry exceeds {} bytes", self.limits.max_entry_bytes),
                });
            }
            copy_result?;
            let (size, digest) = digest_writer.finish();
            total = total
                .checked_add(size)
                .ok_or_else(|| UnrealEngineError::LimitExceeded {
                    input_id: materialized.candidates[candidate_index].input_id.clone(),
                    limit: "expanded byte count overflow".to_owned(),
                })?;
            if total > self.limits.max_expanded_bytes {
                return Err(UnrealEngineError::LimitExceeded {
                    input_id: materialized.candidates[candidate_index].input_id.clone(),
                    limit: format!("expanded bytes exceed {}", self.limits.max_expanded_bytes),
                });
            }
            spool
                .flush()
                .map_err(|source| io_error("flushing entry spool", source))?;
            let candidate = &materialized.candidates[candidate_index];
            let written_path = if size == 0 {
                writer.write_file(&candidate.relative_path, &[])?
            } else {
                // SAFETY: the private spool file remains open and is not modified until the map
                // is dropped at the end of this expression.
                let mapping = unsafe { MmapOptions::new().map(&*spool) }
                    .map_err(|source| io_error("mapping entry spool", source))?;
                writer.write_file(&candidate.relative_path, &mapping)?
            };
            files.push(BuiltFile {
                shadowed_input_ids: conflict_by_path
                    .get(&candidate.key)
                    .cloned()
                    .unwrap_or_default(),
                relative_path: written_path,
                digest,
                size,
                winner_input_id: candidate.input_id.clone(),
            });
        }
        files.sort_by(|left, right| {
            left.relative_path
                .to_ascii_lowercase()
                .cmp(&right.relative_path.to_ascii_lowercase())
        });
        let output = writer.finish()?;
        output
            .sync_all()
            .map_err(|source| io_error("syncing build output", source))?;

        let manifest = UnrealBuildManifest {
            schema_version: 1,
            plan_hash: request.plan_hash,
            build_conditions_digest: conditions_digest,
            target: request.target,
            files,
        };
        verify_path(&output_path, &manifest, self.limits)?;
        let artifact = self
            .artifacts
            .publish_blob_file(&output_path, Some(OUTPUT_MEDIA_TYPE.to_owned()))?;
        let grant = self.artifacts.grant_blob(&artifact)?;
        self.verify(&grant, &manifest)?;
        Ok(UnrealBuildResult {
            artifact,
            manifest,
            conflicts,
        })
    }

    pub fn verify(
        &self,
        artifact: &ArtifactGrant,
        manifest: &UnrealBuildManifest,
    ) -> Result<VerificationReport, UnrealEngineError> {
        let mut file = artifact.open_read()?;
        verify_reader(
            &mut file,
            &artifact.descriptor.digest,
            manifest,
            self.limits,
        )
    }

    fn workspace(&self) -> Result<tempfile::TempDir, UnrealEngineError> {
        Builder::new()
            .prefix("unreal-pak-")
            .tempdir_in(&self.staging)
            .map_err(|source| io_error("creating build workspace", source))
    }
}

fn resolve_winners(materialized: &MaterializedInputs) -> (Vec<usize>, Vec<FileConflict>) {
    struct Claim {
        winner: usize,
        shadowed: Vec<String>,
    }

    let mut claims = BTreeMap::<String, Claim>::new();
    for (index, candidate) in materialized.candidates.iter().enumerate() {
        match claims.get_mut(&candidate.key) {
            Some(claim) => {
                let winner_id = &materialized.candidates[claim.winner].input_id;
                if &candidate.input_id != winner_id && !claim.shadowed.contains(&candidate.input_id)
                {
                    claim.shadowed.push(candidate.input_id.clone());
                }
            }
            None => {
                claims.insert(
                    candidate.key.clone(),
                    Claim {
                        winner: index,
                        shadowed: Vec::new(),
                    },
                );
            }
        }
    }
    let winners = claims
        .values()
        .map(|claim| claim.winner)
        .collect::<Vec<_>>();
    let conflicts = claims
        .into_values()
        .filter(|claim| !claim.shadowed.is_empty())
        .map(|claim| {
            let winner = &materialized.candidates[claim.winner];
            FileConflict {
                relative_path: winner.relative_path.clone(),
                winner_input_id: winner.input_id.clone(),
                shadowed_input_ids: claim.shadowed,
            }
        })
        .collect();
    (winners, conflicts)
}

fn verify_path(
    path: &Path,
    manifest: &UnrealBuildManifest,
    limits: UnrealEngineLimits,
) -> Result<VerificationReport, UnrealEngineError> {
    let mut file = File::open(path).map_err(|source| io_error("opening build output", source))?;
    verify_reader(&mut file, "unpublished", manifest, limits)
}

fn verify_reader(
    file: &mut File,
    artifact_digest: &str,
    manifest: &UnrealBuildManifest,
    limits: UnrealEngineLimits,
) -> Result<VerificationReport, UnrealEngineError> {
    let archive = PakArchive::open(file)?;
    let metadata = archive.metadata().clone();
    if metadata.encrypted_index {
        return Err(UnrealEngineError::Verification(
            "output index is unexpectedly encrypted".to_owned(),
        ));
    }
    if metadata.version != format!("V{}", manifest.target.version) {
        return Err(UnrealEngineError::Verification(format!(
            "expected PAK version {}, found {}",
            manifest.target.version, metadata.version
        )));
    }
    if metadata.mount_point != manifest.target.mount_point {
        return Err(UnrealEngineError::Verification(format!(
            "expected mount point {}, found {}",
            manifest.target.mount_point, metadata.mount_point
        )));
    }
    let expected_codec = match manifest.target.codec {
        mintcat_unreal_pak::PakBuildCodec::None => Vec::new(),
        mintcat_unreal_pak::PakBuildCodec::Zlib => vec![mintcat_unreal_pak::PakCodec::Zlib],
    };
    if metadata.codecs != expected_codec {
        return Err(UnrealEngineError::Verification(format!(
            "expected codecs {expected_codec:?}, found {:?}",
            metadata.codecs
        )));
    }

    let mut expected = BTreeMap::new();
    for file in &manifest.files {
        validate_output_path(&file.relative_path).map_err(|error| {
            UnrealEngineError::Verification(format!("invalid manifest path: {error}"))
        })?;
        if expected
            .insert(file.relative_path.to_ascii_lowercase(), file)
            .is_some()
        {
            return Err(UnrealEngineError::Verification(format!(
                "manifest contains duplicate or case-colliding path {}",
                file.relative_path
            )));
        }
    }
    let mut observed = BTreeSet::new();
    let mut expanded_bytes = 0_u64;
    for stored_path in &metadata.files {
        let path = normalize_entry_path(&metadata.mount_point, stored_path)?;
        let key = path.to_ascii_lowercase();
        if !observed.insert(key.clone()) {
            return Err(UnrealEngineError::Verification(format!(
                "case-colliding output path {path}"
            )));
        }
        let expected_file = expected.get(&key).ok_or_else(|| {
            UnrealEngineError::Verification(format!("unexpected output file {path}"))
        })?;
        let mut digest_writer = LimitedDigestWriter::new(io::sink(), limits.max_entry_bytes);
        let result = archive.read_file_to(stored_path, file, &mut digest_writer);
        if digest_writer.exceeded {
            return Err(UnrealEngineError::LimitExceeded {
                input_id: "build-output".to_owned(),
                limit: format!("entry exceeds {} bytes", limits.max_entry_bytes),
            });
        }
        result?;
        let (size, digest) = digest_writer.finish();
        if size != expected_file.size || digest != expected_file.digest {
            return Err(UnrealEngineError::Verification(format!(
                "output file {path} digest or size differs from build manifest"
            )));
        }
        expanded_bytes = expanded_bytes.checked_add(size).ok_or_else(|| {
            UnrealEngineError::Verification("expanded byte count overflow".to_owned())
        })?;
        if expanded_bytes > limits.max_expanded_bytes {
            return Err(UnrealEngineError::LimitExceeded {
                input_id: "build-output".to_owned(),
                limit: format!("expanded bytes exceed {}", limits.max_expanded_bytes),
            });
        }
    }
    if observed.len() != expected.len() {
        return Err(UnrealEngineError::Verification(format!(
            "expected {} files, observed {}",
            expected.len(),
            observed.len()
        )));
    }
    Ok(VerificationReport {
        artifact_digest: artifact_digest.to_owned(),
        file_count: u64::try_from(observed.len()).map_err(|_| {
            UnrealEngineError::Verification("file count does not fit u64".to_owned())
        })?,
        expanded_bytes,
        metadata,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildConditions<'a> {
    schema_version: u32,
    plan_hash: &'a str,
    target: &'a PakBuildTarget,
    reference_paths: &'a [String],
    inputs: Vec<InputCondition<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InputCondition<'a> {
    input_id: &'a str,
    digest: &'a str,
    size: u64,
    format: UnrealInputFormat,
    content_prefix: Option<&'a str>,
}

fn build_conditions_digest(request: &UnrealBuildRequest<'_>) -> Result<String, UnrealEngineError> {
    let conditions = BuildConditions {
        schema_version: 1,
        plan_hash: &request.plan_hash,
        target: &request.target,
        reference_paths: &request.reference_paths,
        inputs: request
            .inputs
            .iter()
            .map(|input| InputCondition {
                input_id: &input.input_id,
                digest: &input.artifact.descriptor.digest,
                size: input.artifact.descriptor.size,
                format: input.format,
                content_prefix: input.content_prefix.as_deref(),
            })
            .collect(),
    };
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&conditions)?)
    ))
}

fn validate_build_request(request: &UnrealBuildRequest<'_>) -> Result<(), UnrealEngineError> {
    if request.plan_hash.trim().is_empty() {
        return Err(UnrealEngineError::InvalidRequest(
            "plan_hash is required".to_owned(),
        ));
    }
    if request.inputs.is_empty() {
        return Err(UnrealEngineError::InvalidRequest(
            "at least one input is required".to_owned(),
        ));
    }
    Ok(())
}

fn validate_limits(limits: UnrealEngineLimits) -> Result<(), UnrealEngineError> {
    if limits.max_input_bytes == 0
        || limits.max_entries == 0
        || limits.max_entry_bytes == 0
        || limits.max_expanded_bytes == 0
        || limits.max_path_bytes == 0
        || limits.max_compression_ratio == 0
    {
        return Err(UnrealEngineError::InvalidRequest(
            "all engine limits must be positive".to_owned(),
        ));
    }
    Ok(())
}

struct LimitedDigestWriter<W> {
    inner: W,
    hasher: Sha256,
    written: u64,
    limit: u64,
    exceeded: bool,
}

impl<W> LimitedDigestWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            written: 0,
            limit,
            exceeded: false,
        }
    }

    fn finish(self) -> (u64, String) {
        (self.written, format!("sha256:{:x}", self.hasher.finalize()))
    }
}

impl<W: Write> Write for LimitedDigestWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let requested = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        if self.written.saturating_add(requested) > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("Unreal entry exceeds configured limit"));
        }
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.written = self
            .written
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("Unreal entry byte count overflow"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn io_error(scope: impl Into<String>, source: io::Error) -> UnrealEngineError {
    UnrealEngineError::Io {
        scope: scope.into(),
        source,
    }
}

#[cfg(test)]
mod tests;
