use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use mintcat_game_api::{
    build_plan_hash, BuiltGameOutput, DeployPrecondition, GameBuildPlan, GameBuildStep,
    GameDeployFile, GameDeployPlan, GameOutputRole, GameSettingsSnapshot, LegacyFileCandidate,
    LegacyFileDisposition, LegacyInstallReport, PlannedGameInput, PlannedGameOutput,
    ValidatedGameInstallation, GAME_PLAN_SCHEMA_VERSION,
};
use mintcat_loader_ue4ss::DeploymentManifest;
use mintcat_sdk::ArtifactRef;
use serde::{Deserialize, Serialize};

use crate::{
    descriptor::{descriptor, BINARIES_PATH, PAKS_PATH},
    installation::DrgVariant,
    DrgError, DRG_GAME_ID,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DrgCompression {
    None,
    Zlib,
}

impl DrgCompression {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zlib => "zlib",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DrgSettings {
    pub compression: DrgCompression,
    pub ue4ss_enabled: bool,
}

impl Default for DrgSettings {
    fn default() -> Self {
        Self {
            compression: DrgCompression::None,
            ue4ss_enabled: true,
        }
    }
}

impl DrgSettings {
    pub fn snapshot(&self, revision: u64) -> Result<GameSettingsSnapshot, DrgError> {
        Ok(GameSettingsSnapshot::new(
            DRG_GAME_ID,
            1,
            revision,
            serde_json::to_value(self)?,
        )?)
    }

    pub fn from_snapshot(snapshot: &GameSettingsSnapshot) -> Result<Self, DrgError> {
        snapshot.verify()?;
        if snapshot.game_id != DRG_GAME_ID || snapshot.schema_version != 1 {
            return Err(DrgError::InvalidRequest(
                "settings snapshot is not DRG schema version 1".to_owned(),
            ));
        }
        Ok(serde_json::from_value(snapshot.options.clone())?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DrgBuildPlanRequest {
    pub installation: ValidatedGameInstallation,
    pub profile_revision: u64,
    pub module_lock_digest: String,
    pub settings: GameSettingsSnapshot,
    pub base_game_artifact: ArtifactRef,
    pub patch_resource_lock_digest: Option<String>,
    pub ordered_inputs: Vec<PlannedGameInput>,
}

pub fn create_build_plan(request: DrgBuildPlanRequest) -> Result<GameBuildPlan, DrgError> {
    if request.installation.game_id != DRG_GAME_ID {
        return Err(DrgError::InvalidRequest(
            "installation belongs to a different game".to_owned(),
        ));
    }
    let variant = DrgVariant::from_id(&request.installation.variant_id)?;
    let settings = DrgSettings::from_snapshot(&request.settings)?;
    if !request.base_game_artifact.verified
        || request.base_game_artifact.digest != request.installation.fingerprint
        || request.base_game_artifact.size != request.installation.main_pak_size
    {
        return Err(DrgError::InvalidRequest(
            "base game artifact differs from the validated installation fingerprint".to_owned(),
        ));
    }
    if request.module_lock_digest.trim().is_empty() {
        return Err(DrgError::InvalidRequest(
            "module lock digest is required".to_owned(),
        ));
    }
    let patch_resource_lock_digest = request
        .patch_resource_lock_digest
        .filter(|digest| !digest.trim().is_empty());
    let mut input_ids = BTreeSet::new();
    for input in &request.ordered_inputs {
        if input.input_id.trim().is_empty() || !input_ids.insert(input.input_id.clone()) {
            return Err(DrgError::InvalidRequest(format!(
                "duplicate or empty input id {}",
                input.input_id
            )));
        }
        if !input.artifact.verified {
            return Err(DrgError::InvalidRequest(format!(
                "input {} is not a verified artifact",
                input.input_id
            )));
        }
        if input.remote_game_key != "drg" {
            return Err(DrgError::InvalidRequest(format!(
                "input {} has non-DRG source game key {}",
                input.input_id, input.remote_game_key
            )));
        }
    }
    let variant_descriptor = descriptor()
        .variants
        .into_iter()
        .find(|candidate| candidate.variant_id == variant.id())
        .expect("descriptor contains every DRG variant");
    let ordered_ids = request
        .ordered_inputs
        .iter()
        .map(|input| input.input_id.clone())
        .collect::<Vec<_>>();
    let mut steps = vec![
        GameBuildStep::InspectInputs {
            input_ids: ordered_ids.clone(),
        },
        GameBuildStep::GamePatch {
            patch_contract_version: u32::from(patch_resource_lock_digest.is_some()),
            base_game_artifact: request.base_game_artifact,
            resource_lock_digest: patch_resource_lock_digest.clone(),
        },
        GameBuildStep::UnrealPakBuild {
            input_ids: ordered_ids.clone(),
            content_prefix: "FSD".to_owned(),
            version: 11,
            compression: settings.compression.as_str().to_owned(),
        },
    ];
    if settings.ue4ss_enabled {
        steps.push(GameBuildStep::LoaderPlan {
            input_ids: ordered_ids,
            loader_id: "mintcat.loader.ue4ss".to_owned(),
        });
    }
    let mut outputs = vec![PlannedGameOutput {
        role: GameOutputRole::ModPak,
        target_relative_path: variant_descriptor.mod_pak_relative_path,
        owner: "game:drg/integration".to_owned(),
        executable: false,
    }];
    if patch_resource_lock_digest.is_some() {
        outputs.push(PlannedGameOutput {
            role: GameOutputRole::HookDll,
            target_relative_path: format!("{BINARIES_PATH}/x3daudio1_7.dll"),
            owner: "game:drg/patch-resource".to_owned(),
            executable: true,
        });
    }
    let game_pak_stem = match variant {
        DrgVariant::Steam => "FSD-WindowsNoEditor",
        DrgVariant::Xbox => "FSD-WinGDK",
    };
    for input in &request.ordered_inputs {
        if input.audio_only_hint {
            outputs.push(PlannedGameOutput {
                role: GameOutputRole::AudioPak,
                target_relative_path: format!(
                    "{PAKS_PATH}/{}",
                    mintcat_integrator_core::audio_pak_filename(game_pak_stem, &input.display_name)
                ),
                owner: format!("game:drg/audio/{}", input.input_id),
                executable: false,
            });
        }
    }
    let mut output_targets = BTreeSet::new();
    for output in &outputs {
        if !output_targets.insert(output.target_relative_path.to_lowercase()) {
            return Err(DrgError::InvalidRequest(format!(
                "planned output target collision: {}",
                output.target_relative_path
            )));
        }
    }
    let mut plan = GameBuildPlan {
        schema_version: GAME_PLAN_SCHEMA_VERSION,
        plan_hash: String::new(),
        game_id: DRG_GAME_ID.to_owned(),
        variant_id: variant.id().to_owned(),
        game_fingerprint: request.installation.fingerprint,
        profile_revision: request.profile_revision,
        module_lock_digest: request.module_lock_digest,
        settings: request.settings,
        ordered_inputs: request.ordered_inputs,
        steps,
        outputs,
    };
    plan.plan_hash = build_plan_hash(&plan)?;
    Ok(plan)
}

pub fn create_deploy_plan(
    build_plan: &GameBuildPlan,
    built_outputs: Vec<BuiltGameOutput>,
    loader_manifests: Vec<DeploymentManifest>,
    preconditions: BTreeMap<String, DeployPrecondition>,
) -> Result<GameDeployPlan, DrgError> {
    if build_plan.game_id != DRG_GAME_ID || build_plan.plan_hash != build_plan_hash(build_plan)? {
        return Err(DrgError::InvalidRequest(
            "build plan identity or hash is invalid".to_owned(),
        ));
    }
    let deployable_patch = build_plan.steps.iter().any(|step| {
        matches!(
            step,
            GameBuildStep::GamePatch {
                patch_contract_version: 1,
                resource_lock_digest: Some(_),
                ..
            }
        )
    });
    if !deployable_patch {
        return Err(DrgError::InvalidRequest(
            "pack-only DRG build plan cannot become a deployment plan without a signed patch resource"
                .to_owned(),
        ));
    }
    let mut files = Vec::new();
    for output in built_outputs {
        let precondition = find_precondition(&preconditions, &output.target_relative_path)?;
        files.push(GameDeployFile {
            target_relative_path: output.target_relative_path,
            artifact: output.artifact,
            owner: output.owner,
            executable: output.executable,
            precondition,
            approval_kinds: if output.executable {
                vec!["native-code-execution".to_owned()]
            } else {
                Vec::new()
            },
        });
    }
    for manifest in loader_manifests {
        if manifest.game_id != DRG_GAME_ID {
            return Err(DrgError::InvalidRequest(format!(
                "loader manifest belongs to {}",
                manifest.game_id
            )));
        }
        for deployment in manifest.files {
            let precondition = find_precondition(&preconditions, &deployment.target_relative_path)?;
            let approval_kinds = manifest
                .approval_requirements
                .iter()
                .filter(|approval| approval.target_relative_path == deployment.target_relative_path)
                .map(|approval| {
                    serde_json::to_value(&approval.risk)
                        .expect("risk enum serializes")
                        .as_str()
                        .expect("risk enum serializes to string")
                        .to_owned()
                })
                .collect();
            files.push(GameDeployFile {
                target_relative_path: deployment.target_relative_path,
                artifact: deployment.artifact,
                owner: deployment.owner,
                executable: deployment.executable,
                precondition,
                approval_kinds,
            });
        }
    }
    let plan = GameDeployPlan {
        schema_version: GAME_PLAN_SCHEMA_VERSION,
        game_id: DRG_GAME_ID.to_owned(),
        variant_id: build_plan.variant_id.clone(),
        game_fingerprint: build_plan.game_fingerprint.clone(),
        profile_revision: build_plan.profile_revision,
        build_plan_hash: build_plan.plan_hash.clone(),
        module_lock_digest: build_plan.module_lock_digest.clone(),
        files,
    };
    plan.validate()?;
    Ok(plan)
}

pub fn detect_legacy_install(
    root: &Path,
    variant: DrgVariant,
) -> Result<LegacyInstallReport, DrgError> {
    let variant_descriptor = descriptor()
        .variants
        .into_iter()
        .find(|candidate| candidate.variant_id == variant.id())
        .expect("descriptor contains every DRG variant");
    let mut files = Vec::new();
    for relative in [
        "FSD/Content/Paks/mods_P.pak",
        variant_descriptor.mod_pak_relative_path.as_str(),
        "FSD/Binaries/Win64/x3daudio1_7.dll",
        "FSD/Binaries/Win64/UE4SS.dll",
        "FSD/Binaries/Win64/dwmapi.dll",
        "FSD/Binaries/Win64/ue4ss/UE4SS-settings.ini",
    ] {
        let path = root.join(relative);
        if path.is_file() {
            files.push(LegacyFileCandidate {
                relative_path: relative.to_owned(),
                disposition: LegacyFileDisposition::TakeoverRequired,
                reason:
                    "recognized legacy Mint/Mintcat output; backup and approve before replacement"
                        .to_owned(),
            });
        }
    }
    let paks = root.join(PAKS_PATH);
    if paks.is_dir() {
        let mut entries = fs::read_dir(&paks)
            .map_err(|source| DrgError::Io {
                scope: format!("reading legacy DRG PAK directory {}", paks.display()),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| DrgError::Io {
                scope: format!("reading legacy DRG PAK entries {}", paks.display()),
                source,
            })?;
        entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry
                .file_type()
                .map_err(|source| DrgError::Io {
                    scope: format!("inspecting legacy DRG entry {}", entry.path().display()),
                    source,
                })?
                .is_file()
                && name.contains("_audio_")
                && name.ends_with("_P.pak")
            {
                files.push(LegacyFileCandidate {
                    relative_path: format!("{PAKS_PATH}/{name}"),
                    disposition: LegacyFileDisposition::ForeignPreserved,
                    reason: "legacy name pattern alone does not prove receipt ownership".to_owned(),
                });
            }
        }
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(LegacyInstallReport {
        game_id: DRG_GAME_ID.to_owned(),
        variant_id: variant.id().to_owned(),
        requires_approval: files
            .iter()
            .any(|file| file.disposition == LegacyFileDisposition::TakeoverRequired),
        files,
    })
}

fn find_precondition(
    preconditions: &BTreeMap<String, DeployPrecondition>,
    path: &str,
) -> Result<DeployPrecondition, DrgError> {
    let key = path.to_lowercase();
    preconditions.get(&key).cloned().ok_or_else(|| {
        DrgError::InvalidRequest(format!("deploy precondition is missing for {path}"))
    })
}
