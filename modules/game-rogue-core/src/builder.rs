use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use mintcat_engine_unreal_pak::{
    UnrealBuildRequest, UnrealEngineBackend, UnrealEngineLimits, UnrealInput, UnrealInputFormat,
    UnrealPakEngine,
};
use mintcat_game_api::{
    build_plan_hash, BuiltGameOutput, GameBuildPlan, GameBuildStep, GameInputFormat, GameOutputRole,
};
use mintcat_integrator_core::{InstallEvent, InstallProgress, ModInfo};
use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore, ModuleLease};
use mintcat_sdk::{read_directory_snapshot_index, DirectorySnapshotError, DirectorySnapshotLimits};
use mintcat_unreal_pak::{PakBuildCodec, PakBuildTarget, PakMetadata};
use serde::{Deserialize, Serialize};
use tempfile::Builder;
use zip::ZipArchive;

use crate::{
    descriptor::{descriptor, ROGUE_CORE_PATCH_RESOURCE_ID},
    installation::validate_supported_main_pak,
    integrator::pak_integrator::RcPakIntegrator,
    plan::{RogueCoreCompression, RogueCoreSettings},
    RogueCoreError, RogueCoreVariant, ROGUE_CORE_GAME_ID,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RogueCoreBuildLimits {
    pub max_base_pak_bytes: u64,
    pub max_input_bytes: u64,
    pub max_entries: u64,
    pub max_entry_bytes: u64,
    pub max_expanded_bytes: u64,
    pub max_path_bytes: usize,
    pub max_compression_ratio: u64,
}

impl Default for RogueCoreBuildLimits {
    fn default() -> Self {
        Self {
            max_base_pak_bytes: 16 * 1024 * 1024 * 1024,
            max_input_bytes: 4 * 1024 * 1024 * 1024,
            max_entries: 100_000,
            max_entry_bytes: 2 * 1024 * 1024 * 1024,
            max_expanded_bytes: 16 * 1024 * 1024 * 1024,
            max_path_bytes: 4 * 1024,
            max_compression_ratio: 1_000,
        }
    }
}

pub struct RogueCoreBuildInput<'a> {
    pub input_id: String,
    pub artifact: &'a ArtifactGrant,
}

pub struct RogueCorePatchPackage<'a> {
    pub package: &'a ModuleLease,
    pub archive_path: String,
}

pub struct RogueCoreBuildRequest<'a> {
    pub plan: &'a GameBuildPlan,
    pub base_game_artifact: &'a ArtifactGrant,
    pub inputs: Vec<RogueCoreBuildInput<'a>>,
    pub patch_package: Option<RogueCorePatchPackage<'a>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RogueCoreBuildManifest {
    pub schema_version: u32,
    pub plan_hash: String,
    pub base_game_digest: String,
    pub patch_package_digest: Option<String>,
    pub ordered_input_digests: Vec<String>,
    pub outputs: Vec<BuiltGameOutput>,
    pub pak_metadata: BTreeMap<String, PakMetadata>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RogueCoreBuildResult {
    pub outputs: Vec<BuiltGameOutput>,
    pub manifest: RogueCoreBuildManifest,
}

#[derive(Clone)]
pub struct RogueCoreBuilder<E: UnrealEngineBackend = UnrealPakEngine> {
    artifacts: ArtifactStore,
    engine: E,
    staging: PathBuf,
    limits: RogueCoreBuildLimits,
}

impl RogueCoreBuilder {
    pub fn open(
        artifacts: ArtifactStore,
        staging: impl Into<PathBuf>,
        limits: RogueCoreBuildLimits,
    ) -> Result<Self, RogueCoreError> {
        let staging = staging.into();
        let engine = UnrealPakEngine::open(
            artifacts.clone(),
            staging.join("unreal-engine"),
            UnrealEngineLimits {
                max_input_bytes: limits.max_input_bytes,
                max_entries: limits.max_entries,
                max_entry_bytes: limits.max_entry_bytes,
                max_expanded_bytes: limits.max_expanded_bytes,
                max_path_bytes: limits.max_path_bytes,
                max_compression_ratio: limits.max_compression_ratio,
            },
        )?;
        Self::open_with_engine(artifacts, staging, limits, engine)
    }
}

impl<E: UnrealEngineBackend> RogueCoreBuilder<E> {
    pub fn open_with_engine(
        artifacts: ArtifactStore,
        staging: impl Into<PathBuf>,
        limits: RogueCoreBuildLimits,
        engine: E,
    ) -> Result<Self, RogueCoreError> {
        validate_limits(limits)?;
        let staging = staging.into();
        fs::create_dir_all(&staging).map_err(|source| RogueCoreError::Io {
            scope: format!("creating Rogue Core staging {}", staging.display()),
            source,
        })?;
        let metadata = fs::symlink_metadata(&staging).map_err(|source| RogueCoreError::Io {
            scope: format!("inspecting Rogue Core staging {}", staging.display()),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(RogueCoreError::InvalidRequest(
                "Rogue Core staging must be a real directory".to_owned(),
            ));
        }
        Ok(Self { artifacts, engine, staging, limits })
    }

    pub fn build(
        &self,
        request: RogueCoreBuildRequest<'_>,
    ) -> Result<RogueCoreBuildResult, RogueCoreError> {
        validate_plan(request.plan)?;
        let settings = RogueCoreSettings::from_snapshot(&request.plan.settings)?;
        let variant = RogueCoreVariant::from_id(&request.plan.variant_id)?;
        let (expected_base, expected_patch) = patch_step(request.plan)?;
        validate_grant(
            request.base_game_artifact,
            &expected_base.digest,
            expected_base.size,
            "base-game",
            self.limits.max_base_pak_bytes,
        )?;

        let mut granted_inputs = BTreeMap::new();
        for input in request.inputs {
            if granted_inputs
                .insert(input.input_id.clone(), input.artifact)
                .is_some()
            {
                return Err(RogueCoreError::InvalidRequest(format!(
                    "duplicate granted input {}",
                    input.input_id
                )));
            }
        }
        if granted_inputs.len() != request.plan.ordered_inputs.len() {
            return Err(RogueCoreError::InvalidRequest(
                "granted input count differs from build plan".to_owned(),
            ));
        }

        let workspace = Builder::new()
            .prefix("rogue-core-build-")
            .tempdir_in(&self.staging)
            .map_err(|source| RogueCoreError::Io {
                scope: "creating Rogue Core build workspace".to_owned(),
                source,
            })?;
        let game_root = workspace.path().join("game");
        let variant_descriptor = descriptor()
            .variants
            .into_iter()
            .find(|candidate| candidate.variant_id == variant.id())
            .expect("descriptor contains every RogueCoreVariant");
        fs::create_dir_all(game_root.join(&variant_descriptor.paks_relative_path)).map_err(
            |source| RogueCoreError::Io {
                scope: "creating staged Rogue Core PAK directory".to_owned(),
                source,
            },
        )?;
        fs::create_dir_all(game_root.join(&variant_descriptor.binaries_relative_path)).map_err(
            |source| RogueCoreError::Io {
                scope: "creating staged Rogue Core binaries directory".to_owned(),
                source,
            },
        )?;
        let base_path = game_root.join(&variant_descriptor.main_pak_relative_path);
        copy_grant_to(
            request.base_game_artifact,
            &base_path,
            expected_base.size,
            "base-game",
        )?;
        validate_supported_main_pak(&base_path)?;

        let inputs_root = workspace.path().join("inputs");
        fs::create_dir(&inputs_root).map_err(|source| RogueCoreError::Io {
            scope: "creating Rogue Core input staging".to_owned(),
            source,
        })?;
        let mut legacy_inputs = Vec::with_capacity(request.plan.ordered_inputs.len());
        let mut ordered_input_digests = Vec::with_capacity(request.plan.ordered_inputs.len());
        for (index, planned) in request.plan.ordered_inputs.iter().enumerate() {
            let grant = granted_inputs.remove(&planned.input_id).ok_or_else(|| {
                RogueCoreError::InvalidRequest(format!(
                    "grant is missing for planned input {}",
                    planned.input_id
                ))
            })?;
            validate_grant(
                grant,
                &planned.artifact.digest,
                planned.artifact.size,
                &planned.input_id,
                self.limits.max_input_bytes,
            )?;
            let (path, unpacked) = match planned.format {
                GameInputFormat::Pak | GameInputFormat::Zip => {
                    let extension = if planned.format == GameInputFormat::Pak {
                        "pak"
                    } else {
                        "zip"
                    };
                    let path = inputs_root.join(format!("{index}.{extension}"));
                    copy_grant_to(grant, &path, planned.artifact.size, &planned.input_id)?;
                    (path, false)
                }
                GameInputFormat::DirectorySnapshot => {
                    let path = inputs_root.join(format!("directory-{index}"));
                    materialize_snapshot(grant, &path, &planned.input_id, self.limits)?;
                    (path, true)
                }
            };
            legacy_inputs.push(ModInfo {
                modio_id: planned.remote_mod_id.parse().ok(),
                name: planned.display_name.clone(),
                pak_path: path.to_string_lossy().into_owned(),
                is_unpacked: unpacked,
                is_audio_only: planned.audio_only_hint,
            });
            ordered_input_digests.push(planned.artifact.digest.clone());
        }

        let patch_path = match (expected_patch.as_deref(), request.patch_package) {
            (Some(expected), Some(package)) => {
                Some(validate_patch_package(package, expected, self.limits)?)
            }
            (None, None) => None,
            (Some(_), None) => {
                return Err(RogueCoreError::InvalidRequest(
                    "signed Rogue Core patch package required by plan was not granted".to_owned(),
                ));
            }
            (None, Some(_)) => {
                return Err(RogueCoreError::InvalidRequest(
                    "patch package was supplied to a pack-only plan".to_owned(),
                ));
            }
        };

        RcPakIntegrator::new(
            &base_path,
            settings.compression == RogueCoreCompression::Zlib,
        )
        .map_err(|error| RogueCoreError::CompatibilityBuild(error.to_string()))?
        .install(
            &SilentProgress,
            &mut legacy_inputs,
            true,
            None,
            patch_path.as_deref(),
        )
        .map_err(|error| RogueCoreError::CompatibilityBuild(error.to_string()))?;

        verify_staged_file_set(
            &game_root,
            &variant_descriptor.main_pak_relative_path,
            &request.plan.outputs,
        )?;
        let mut outputs = Vec::with_capacity(request.plan.outputs.len());
        let mut pak_metadata = BTreeMap::new();
        for planned in &request.plan.outputs {
            let source = game_root.join(&planned.target_relative_path);
            let metadata = fs::symlink_metadata(&source).map_err(|error| {
                RogueCoreError::Verification(format!(
                    "planned output {} is absent: {error}",
                    planned.target_relative_path
                ))
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(RogueCoreError::Verification(format!(
                    "planned output {} is not a regular file",
                    planned.target_relative_path
                )));
            }
            let media_type = match planned.role {
                GameOutputRole::ModPak | GameOutputRole::AudioPak => {
                    Some("application/vnd.unreal.pak".to_owned())
                }
                GameOutputRole::HookDll => {
                    Some("application/vnd.microsoft.portable-executable".to_owned())
                }
                GameOutputRole::LoaderFile => Some("application/octet-stream".to_owned()),
            };
            let mut artifact = self.artifacts.publish_blob_file(&source, media_type)?;
            if matches!(
                planned.role,
                GameOutputRole::ModPak | GameOutputRole::AudioPak
            ) {
                let grant = self.artifacts.grant_blob(&artifact)?;
                let inspection = self.engine.inspect(UnrealInput {
                    input_id: planned.target_relative_path.clone(),
                    artifact: &grant,
                    format: UnrealInputFormat::Pak,
                    content_prefix: None,
                })?;
                let metadata = &inspection
                    .pak_containers
                    .first()
                    .ok_or_else(|| {
                        RogueCoreError::Verification(format!(
                            "output {} has no PAK metadata",
                            planned.target_relative_path
                        ))
                    })?
                    .metadata;
                let version = metadata.version.strip_prefix('V')
                    .and_then(|value| value.parse::<u8>().ok())
                    .ok_or_else(|| RogueCoreError::Verification(format!(
                        "output {} has an unsupported PAK version {}",
                        planned.target_relative_path, metadata.version
                    )))?;
                let built = self.engine.build(UnrealBuildRequest {
                    plan_hash: request.plan.plan_hash.clone(),
                    inputs: vec![UnrealInput {
                        input_id: planned.target_relative_path.clone(),
                        artifact: &grant,
                        format: UnrealInputFormat::Pak,
                        content_prefix: None,
                    }],
                    reference_paths: inspection.files,
                    target: PakBuildTarget {
                        version,
                        mount_point: metadata.mount_point.clone(),
                        codec: if settings.compression == RogueCoreCompression::Zlib {
                            PakBuildCodec::Zlib
                        } else {
                            PakBuildCodec::None
                        },
                    },
                })?;
                self.engine.verify(&grant, &built.manifest)?;
                let built_grant = self.artifacts.grant_blob(&built.artifact)?;
                let verified = self.engine.verify(&built_grant, &built.manifest)?;
                artifact = built.artifact;
                let metadata = verified.metadata;
                pak_metadata.insert(planned.target_relative_path.clone(), metadata);
            }
            outputs.push(BuiltGameOutput {
                role: planned.role.clone(),
                target_relative_path: planned.target_relative_path.clone(),
                artifact,
                owner: planned.owner.clone(),
                executable: planned.executable,
            });
        }
        let observed = outputs
            .iter()
            .map(|output| output.target_relative_path.to_lowercase())
            .collect::<BTreeSet<_>>();
        let expected = request
            .plan
            .outputs
            .iter()
            .map(|output| output.target_relative_path.to_lowercase())
            .collect::<BTreeSet<_>>();
        if observed != expected {
            return Err(RogueCoreError::Verification(
                "published output set differs from build plan".to_owned(),
            ));
        }
        let manifest = RogueCoreBuildManifest {
            schema_version: 1,
            plan_hash: request.plan.plan_hash.clone(),
            base_game_digest: expected_base.digest,
            patch_package_digest: expected_patch,
            ordered_input_digests,
            outputs: outputs.clone(),
            pak_metadata,
        };
        Ok(RogueCoreBuildResult { outputs, manifest })
    }
}

struct SilentProgress;

impl InstallProgress for SilentProgress {
    fn emit(&self, _: InstallEvent) -> anyhow::Result<()> {
        Ok(())
    }
}

fn validate_plan(plan: &GameBuildPlan) -> Result<(), RogueCoreError> {
    if plan.schema_version != 1
        || plan.game_id != ROGUE_CORE_GAME_ID
        || plan.plan_hash != build_plan_hash(plan)?
    {
        return Err(RogueCoreError::InvalidRequest(
            "Rogue Core build plan schema, game, or hash is invalid".to_owned(),
        ));
    }
    RogueCoreSettings::from_snapshot(&plan.settings)?;
    RogueCoreVariant::from_id(&plan.variant_id)?;
    Ok(())
}

fn patch_step(
    plan: &GameBuildPlan,
) -> Result<(mintcat_sdk::ArtifactRef, Option<String>), RogueCoreError> {
    let mut matches = plan.steps.iter().filter_map(|step| match step {
        GameBuildStep::GamePatch {
            patch_contract_version,
            base_game_artifact,
            resource_lock_digest,
        } => Some((
            *patch_contract_version,
            base_game_artifact.clone(),
            resource_lock_digest.clone(),
        )),
        _ => None,
    });
    let Some((version, base, resource)) = matches.next() else {
        return Err(RogueCoreError::InvalidRequest(
            "Rogue Core build plan lacks its game patch step".to_owned(),
        ));
    };
    if matches.next().is_some() || !matches!((version, resource.as_ref()), (0, None) | (1, Some(_)))
    {
        return Err(RogueCoreError::InvalidRequest(
            "Rogue Core game patch step is ambiguous or unsupported".to_owned(),
        ));
    }
    Ok((base, resource))
}

fn validate_grant(
    grant: &ArtifactGrant,
    expected_digest: &str,
    expected_size: u64,
    input_id: &str,
    max_bytes: u64,
) -> Result<(), RogueCoreError> {
    if expected_size == 0 || expected_size > max_bytes {
        return Err(RogueCoreError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: format!("artifact must contain between 1 and {max_bytes} bytes"),
        });
    }
    if grant.descriptor.digest != expected_digest || grant.descriptor.size != expected_size {
        return Err(RogueCoreError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: "artifact grant differs from immutable build plan".to_owned(),
        });
    }
    if grant
        .open_read()?
        .metadata()
        .map_err(|source| RogueCoreError::Io {
            scope: format!("inspecting granted Rogue Core input {input_id}"),
            source,
        })?
        .len()
        != expected_size
    {
        return Err(RogueCoreError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: "granted file size differs".to_owned(),
        });
    }
    Ok(())
}

fn copy_grant_to(
    grant: &ArtifactGrant,
    destination: &Path,
    expected_size: u64,
    input_id: &str,
) -> Result<(), RogueCoreError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|source| RogueCoreError::Io {
            scope: format!("creating staging parent for {input_id}"),
            source,
        })?;
    }
    let mut source = grant.open_read()?;
    let mut output = create_new(destination, &format!("staging {input_id}"))?;
    let copied = io::copy(&mut source, &mut output).map_err(|source| RogueCoreError::Io {
        scope: format!("copying granted input {input_id}"),
        source,
    })?;
    if copied != expected_size {
        return Err(RogueCoreError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: format!("copied {copied} bytes, expected {expected_size}"),
        });
    }
    output.sync_all().map_err(|source| RogueCoreError::Io {
        scope: format!("syncing staged input {input_id}"),
        source,
    })?;
    Ok(())
}

fn materialize_snapshot(
    grant: &ArtifactGrant,
    destination: &Path,
    input_id: &str,
    limits: RogueCoreBuildLimits,
) -> Result<(), RogueCoreError> {
    fs::create_dir(destination).map_err(|source| RogueCoreError::Io {
        scope: format!("creating directory input {input_id}"),
        source,
    })?;
    let mut source = grant.open_read()?;
    let snapshot = read_directory_snapshot_index(
        &mut source,
        DirectorySnapshotLimits {
            max_entries: limits.max_entries,
            max_path_bytes: limits.max_path_bytes,
            max_entry_bytes: limits.max_entry_bytes,
            max_total_bytes: limits.max_expanded_bytes,
        },
    )
    .map_err(|error| map_snapshot_error(input_id, error))?;
    for entry in snapshot.entries {
        validate_portable_path(&entry.relative_path, limits.max_path_bytes).map_err(|reason| {
            RogueCoreError::InvalidInput {
                input_id: input_id.to_owned(),
                reason,
            }
        })?;
        let target = destination.join(&entry.relative_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| RogueCoreError::Io {
                scope: format!("creating snapshot path for {input_id}"),
                source,
            })?;
        }
        source
            .seek(SeekFrom::Start(entry.content_offset))
            .map_err(|source| RogueCoreError::Io {
                scope: format!("seeking snapshot input {input_id}"),
                source,
            })?;
        let mut output = create_new(&target, &format!("materializing snapshot {input_id}"))?;
        let copied = io::copy(&mut Read::by_ref(&mut source).take(entry.size), &mut output)
            .map_err(|source| RogueCoreError::Io {
                scope: format!("copying snapshot input {input_id}"),
                source,
            })?;
        if copied != entry.size {
            return Err(RogueCoreError::Archive {
                input_id: input_id.to_owned(),
                reason: format!(
                    "snapshot entry {} ended at {copied} of {} bytes",
                    entry.relative_path, entry.size
                ),
            });
        }
    }
    Ok(())
}

fn validate_patch_package(
    package: RogueCorePatchPackage<'_>,
    expected_digest: &str,
    limits: RogueCoreBuildLimits,
) -> Result<PathBuf, RogueCoreError> {
    let installed = package.package.module();
    if installed.digest != expected_digest
        || installed.manifest.id != ROGUE_CORE_PATCH_RESOURCE_ID
        || installed.manifest.files.len() != 1
    {
        return Err(RogueCoreError::InvalidRequest(
            "Rogue Core patch package identity, digest, or file set differs from plan".to_owned(),
        ));
    }
    let signed_file = installed.manifest.files.first().expect("length checked");
    if signed_file.path != package.archive_path || signed_file.executable {
        return Err(RogueCoreError::InvalidRequest(
            "Rogue Core patch archive must be the package's sole non-executable signed file"
                .to_owned(),
        ));
    }
    let path = installed.entry_path(&package.archive_path)?;
    validate_patch_archive(&path, limits)?;
    Ok(path)
}

pub(crate) fn validate_patch_archive(
    path: &Path,
    limits: RogueCoreBuildLimits,
) -> Result<(), RogueCoreError> {
    let metadata = fs::metadata(path).map_err(|source| RogueCoreError::Io {
        scope: "inspecting signed Rogue Core patch archive".to_owned(),
        source,
    })?;
    if metadata.len() == 0 || metadata.len() > limits.max_input_bytes {
        return Err(RogueCoreError::InvalidInput {
            input_id: "rogue-core-patch-resource".to_owned(),
            reason: "patch archive size is outside configured limits".to_owned(),
        });
    }
    let file = File::open(path).map_err(|source| RogueCoreError::Io {
        scope: "opening signed Rogue Core patch archive".to_owned(),
        source,
    })?;
    let mut archive = ZipArchive::new(file).map_err(|error| RogueCoreError::Archive {
        input_id: "rogue-core-patch-resource".to_owned(),
        reason: error.to_string(),
    })?;
    if archive.len() as u64 > limits.max_entries {
        return Err(RogueCoreError::InvalidInput {
            input_id: "rogue-core-patch-resource".to_owned(),
            reason: format!("entry count exceeds {}", limits.max_entries),
        });
    }
    let mut paths = BTreeSet::new();
    let mut expanded = 0_u64;
    let mut payload_files = 0_u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| RogueCoreError::Archive {
                input_id: "rogue-core-patch-resource".to_owned(),
                reason: error.to_string(),
            })?;
        if entry.unix_mode().is_some_and(|mode| {
            let file_type = mode & 0o170000;
            file_type != 0 && file_type != 0o100000 && !(file_type == 0o040000 && entry.is_dir())
        }) {
            return Err(RogueCoreError::Archive {
                input_id: "rogue-core-patch-resource".to_owned(),
                reason: "archive contains a link or special file".to_owned(),
            });
        }
        let raw = std::str::from_utf8(entry.name_raw()).map_err(|_| RogueCoreError::Archive {
            input_id: "rogue-core-patch-resource".to_owned(),
            reason: "archive path is not UTF-8".to_owned(),
        })?;
        let normalized = validate_portable_path(raw.trim_end_matches('/'), limits.max_path_bytes)
            .map_err(|reason| RogueCoreError::Archive {
            input_id: "rogue-core-patch-resource".to_owned(),
            reason,
        })?;
        if !paths.insert(normalized.to_lowercase()) {
            return Err(RogueCoreError::Archive {
                input_id: "rogue-core-patch-resource".to_owned(),
                reason: format!("duplicate or case-colliding path {normalized}"),
            });
        }
        if entry.is_dir() {
            continue;
        }
        if !normalized.starts_with("Paks/") {
            return Err(RogueCoreError::Archive {
                input_id: "rogue-core-patch-resource".to_owned(),
                reason: format!("undeclared patch resource path {normalized}"),
            });
        }
        if entry.size() == 0
            || entry.size() > limits.max_entry_bytes
            || (entry.compressed_size() == 0
                || entry.size()
                    > entry
                        .compressed_size()
                        .saturating_mul(limits.max_compression_ratio))
        {
            return Err(RogueCoreError::InvalidInput {
                input_id: "rogue-core-patch-resource".to_owned(),
                reason: format!("entry {normalized} exceeds size or compression limits"),
            });
        }
        expanded =
            expanded
                .checked_add(entry.size())
                .ok_or_else(|| RogueCoreError::InvalidInput {
                    input_id: "rogue-core-patch-resource".to_owned(),
                    reason: "expanded size overflow".to_owned(),
                })?;
        if expanded > limits.max_expanded_bytes {
            return Err(RogueCoreError::InvalidInput {
                input_id: "rogue-core-patch-resource".to_owned(),
                reason: "expanded size exceeds configured limit".to_owned(),
            });
        }
        payload_files += 1;
    }
    if payload_files == 0 {
        return Err(RogueCoreError::Archive {
            input_id: "rogue-core-patch-resource".to_owned(),
            reason: "patch archive contains no Paks payload".to_owned(),
        });
    }
    Ok(())
}

fn verify_staged_file_set(
    game_root: &Path,
    base_relative: &str,
    outputs: &[mintcat_game_api::PlannedGameOutput],
) -> Result<(), RogueCoreError> {
    let mut allowed = outputs
        .iter()
        .map(|output| output.target_relative_path.to_lowercase())
        .collect::<BTreeSet<_>>();
    allowed.insert(base_relative.to_lowercase());
    let mut actual = Vec::new();
    collect_files(game_root, game_root, &mut actual)?;
    for path in actual {
        if !allowed.contains(&path.to_lowercase()) {
            return Err(RogueCoreError::Verification(format!(
                "compatibility builder emitted undeclared file {path}"
            )));
        }
    }
    Ok(())
}

fn collect_files(
    root: &Path,
    directory: &Path,
    result: &mut Vec<String>,
) -> Result<(), RogueCoreError> {
    let mut entries = fs::read_dir(directory)
        .map_err(|source| RogueCoreError::Io {
            scope: format!("reading staged directory {}", directory.display()),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| RogueCoreError::Io {
            scope: format!("reading staged entries {}", directory.display()),
            source,
        })?;
    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());
    for entry in entries {
        let file_type = entry.file_type().map_err(|source| RogueCoreError::Io {
            scope: format!("inspecting staged path {}", entry.path().display()),
            source,
        })?;
        if file_type.is_symlink() {
            return Err(RogueCoreError::Verification(format!(
                "compatibility builder emitted symlink {}",
                entry.path().display()
            )));
        }
        if file_type.is_dir() {
            collect_files(root, &entry.path(), result)?;
        } else if file_type.is_file() {
            result.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .expect("walk remains below root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        } else {
            return Err(RogueCoreError::Verification(format!(
                "compatibility builder emitted special file {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn validate_portable_path(value: &str, max_bytes: usize) -> Result<String, String> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.starts_with(['/', '\\'])
        || value.contains('\\')
        || value.contains(':')
        || value.contains('\0')
    {
        return Err("path is not portable and relative".to_owned());
    }
    let mut normalized = String::new();
    for component in value.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with(['.', ' '])
            || component.chars().any(|character| {
                character.is_control() || matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
            })
        {
            return Err(format!("invalid path component in {value}"));
        }
        let stem = component.split('.').next().unwrap_or(component);
        let upper = stem.to_ascii_uppercase();
        if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || upper.strip_prefix("COM").is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
            || upper.strip_prefix("LPT").is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
        {
            return Err(format!("reserved Windows path component {component}"));
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component);
    }
    Ok(normalized)
}

fn create_new(path: &Path, scope: &str) -> Result<File, RogueCoreError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| RogueCoreError::Io {
            scope: scope.to_owned(),
            source,
        })
}

fn map_snapshot_error(input_id: &str, error: DirectorySnapshotError) -> RogueCoreError {
    match error {
        DirectorySnapshotError::Limit(reason) | DirectorySnapshotError::Invalid(reason) => {
            RogueCoreError::InvalidInput {
                input_id: input_id.to_owned(),
                reason,
            }
        }
        DirectorySnapshotError::Io(source) => RogueCoreError::Io {
            scope: format!("reading directory snapshot {input_id}"),
            source,
        },
    }
}

fn validate_limits(limits: RogueCoreBuildLimits) -> Result<(), RogueCoreError> {
    if limits.max_base_pak_bytes == 0
        || limits.max_input_bytes == 0
        || limits.max_entries == 0
        || limits.max_entry_bytes == 0
        || limits.max_expanded_bytes == 0
        || limits.max_path_bytes == 0
        || limits.max_compression_ratio == 0
    {
        return Err(RogueCoreError::InvalidRequest(
            "all Rogue Core build limits must be positive".to_owned(),
        ));
    }
    Ok(())
}
