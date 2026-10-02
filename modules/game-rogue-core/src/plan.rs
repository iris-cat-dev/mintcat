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
    descriptor::{MOD_PAK, PAKS_PATH},
    installation::RogueCoreVariant,
    RogueCoreError, ROGUE_CORE_GAME_ID,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RogueCoreCompression {
    None,
    Zlib,
}

impl RogueCoreCompression {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zlib => "zlib",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RogueCoreSettings {
    pub compression: RogueCoreCompression,
    pub ue4ss_enabled: bool,
}

impl Default for RogueCoreSettings {
    fn default() -> Self {
        Self {
            compression: RogueCoreCompression::None,
            ue4ss_enabled: true,
        }
    }
}

impl RogueCoreSettings {
    pub fn snapshot(&self, revision: u64) -> Result<GameSettingsSnapshot, RogueCoreError> {
        Ok(GameSettingsSnapshot::new(
            ROGUE_CORE_GAME_ID,
            1,
            revision,
            serde_json::to_value(self)?,
        )?)
    }

    pub fn from_snapshot(snapshot: &GameSettingsSnapshot) -> Result<Self, RogueCoreError> {
        snapshot.verify()?;
        if snapshot.game_id != ROGUE_CORE_GAME_ID || snapshot.schema_version != 1 {
            return Err(RogueCoreError::InvalidRequest(
                "settings snapshot is not Rogue Core schema version 1".to_owned(),
            ));
        }
        Ok(serde_json::from_value(snapshot.options.clone())?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RogueCoreBuildPlanRequest {
    pub installation: ValidatedGameInstallation,
    pub profile_revision: u64,
    pub module_lock_digest: String,
    pub settings: GameSettingsSnapshot,
    pub base_game_artifact: ArtifactRef,
    pub patch_resource_lock_digest: Option<String>,
    pub ordered_inputs: Vec<PlannedGameInput>,
}

pub fn create_build_plan(
    request: RogueCoreBuildPlanRequest,
) -> Result<GameBuildPlan, RogueCoreError> {
    if request.installation.game_id != ROGUE_CORE_GAME_ID {
        return Err(RogueCoreError::InvalidRequest(
            "installation belongs to a different game".to_owned(),
        ));
    }
    RogueCoreVariant::from_id(&request.installation.variant_id)?;
    let settings = RogueCoreSettings::from_snapshot(&request.settings)?;
    if !request.base_game_artifact.verified
        || request.base_game_artifact.digest != request.installation.fingerprint
        || request.base_game_artifact.size != request.installation.main_pak_size
    {
        return Err(RogueCoreError::InvalidRequest(
            "base game artifact differs from the validated installation fingerprint".to_owned(),
        ));
    }
    if request.module_lock_digest.trim().is_empty() {
        return Err(RogueCoreError::InvalidRequest(
            "module lock digest is required".to_owned(),
        ));
    }
    let patch_resource_lock_digest = request
        .patch_resource_lock_digest
        .filter(|digest| !digest.trim().is_empty());
    let mut input_ids = BTreeSet::new();
    for input in &request.ordered_inputs {
        if input.input_id.trim().is_empty() || !input_ids.insert(input.input_id.clone()) {
            return Err(RogueCoreError::InvalidRequest(format!(
                "duplicate or empty input id {}",
                input.input_id
            )));
        }
        if !input.artifact.verified {
            return Err(RogueCoreError::InvalidRequest(format!(
                "input {} is not a verified artifact",
                input.input_id
            )));
        }
        if input.remote_game_key != "rogue-core" {
            return Err(RogueCoreError::InvalidRequest(format!(
                "input {} has non-Rogue-Core source game key {}",
                input.input_id, input.remote_game_key
            )));
        }
    }
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
            resource_lock_digest: patch_resource_lock_digest,
        },
        GameBuildStep::UnrealPakBuild {
            input_ids: ordered_ids.clone(),
            content_prefix: "RogueCore".to_owned(),
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
        target_relative_path: MOD_PAK.to_owned(),
        owner: "game:rogue-core/integration".to_owned(),
        executable: false,
    }];
    for input in &request.ordered_inputs {
        if input.audio_only_hint {
            outputs.push(PlannedGameOutput {
                role: GameOutputRole::AudioPak,
                target_relative_path: format!(
                    "{PAKS_PATH}/{}",
                    mintcat_integrator_core::audio_pak_filename(
                        "RogueCore-Windows",
                        &input.display_name
                    )
                ),
                owner: format!("game:rogue-core/audio/{}", input.input_id),
                executable: false,
            });
        }
    }
    let mut targets = BTreeSet::new();
    for output in &outputs {
        if !targets.insert(output.target_relative_path.to_lowercase()) {
            return Err(RogueCoreError::InvalidRequest(format!(
                "planned output target collision: {}",
                output.target_relative_path
            )));
        }
    }
    let mut plan = GameBuildPlan {
        schema_version: GAME_PLAN_SCHEMA_VERSION,
        plan_hash: String::new(),
        game_id: ROGUE_CORE_GAME_ID.to_owned(),
        variant_id: request.installation.variant_id,
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
) -> Result<GameDeployPlan, RogueCoreError> {
    if build_plan.game_id != ROGUE_CORE_GAME_ID
        || build_plan.plan_hash != build_plan_hash(build_plan)?
    {
        return Err(RogueCoreError::InvalidRequest(
            "build plan identity or hash is invalid".to_owned(),
        ));
    }
    if !build_plan.steps.iter().any(|step| {
        matches!(
            step,
            GameBuildStep::GamePatch {
                patch_contract_version: 1,
                resource_lock_digest: Some(_),
                ..
            }
        )
    }) {
        return Err(RogueCoreError::InvalidRequest(
            "pack-only Rogue Core plan cannot become a deployment plan without a signed patch resource"
                .to_owned(),
        ));
    }
    let mut files = Vec::new();
    for output in built_outputs {
        files.push(GameDeployFile {
            precondition: find_precondition(&preconditions, &output.target_relative_path)?,
            target_relative_path: output.target_relative_path,
            artifact: output.artifact,
            owner: output.owner,
            executable: output.executable,
            approval_kinds: if output.executable {
                vec!["native-code-execution".to_owned()]
            } else {
                Vec::new()
            },
        });
    }
    for manifest in loader_manifests {
        if manifest.game_id != ROGUE_CORE_GAME_ID {
            return Err(RogueCoreError::InvalidRequest(format!(
                "loader manifest belongs to {}",
                manifest.game_id
            )));
        }
        for deployment in manifest.files {
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
                precondition: find_precondition(&preconditions, &deployment.target_relative_path)?,
                target_relative_path: deployment.target_relative_path,
                artifact: deployment.artifact,
                owner: deployment.owner,
                executable: deployment.executable,
                approval_kinds,
            });
        }
    }
    let plan = GameDeployPlan {
        schema_version: GAME_PLAN_SCHEMA_VERSION,
        game_id: ROGUE_CORE_GAME_ID.to_owned(),
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
    variant: RogueCoreVariant,
) -> Result<LegacyInstallReport, RogueCoreError> {
    let mut files = Vec::new();
    for relative in [
        MOD_PAK,
        "Binaries/Win64/UE4SS.dll",
        "Binaries/Win64/dwmapi.dll",
        "Binaries/Win64/ue4ss/UE4SS-settings.ini",
    ] {
        if root.join(relative).is_file() {
            files.push(LegacyFileCandidate {
                relative_path: relative.to_owned(),
                disposition: LegacyFileDisposition::TakeoverRequired,
                reason: "recognized legacy Mintcat output; backup and approve before replacement"
                    .to_owned(),
            });
        }
    }
    let paks = root.join(PAKS_PATH);
    if paks.is_dir() {
        let mut entries = fs::read_dir(&paks)
            .map_err(|source| RogueCoreError::Io {
                scope: format!("reading legacy Rogue Core PAK directory {}", paks.display()),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| RogueCoreError::Io {
                scope: format!("reading legacy Rogue Core entries {}", paks.display()),
                source,
            })?;
        entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry
                .file_type()
                .map_err(|source| RogueCoreError::Io {
                    scope: format!(
                        "inspecting legacy Rogue Core entry {}",
                        entry.path().display()
                    ),
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
        game_id: ROGUE_CORE_GAME_ID.to_owned(),
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
) -> Result<DeployPrecondition, RogueCoreError> {
    preconditions
        .get(&path.to_lowercase())
        .cloned()
        .ok_or_else(|| {
            RogueCoreError::InvalidRequest(format!("deploy precondition is missing for {path}"))
        })
}
