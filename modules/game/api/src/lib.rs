mod execution;
pub use execution::*;

use std::collections::{BTreeMap, BTreeSet};

use mintcat_sdk::ArtifactRef;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

pub const GAME_ADAPTER_CONTRACT_ID: &str = "mintcat.game-adapter";
pub const GAME_ADAPTER_CONTRACT_VERSION: u64 = 2;
pub const GAME_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum GameContractError {
    #[error("invalid game contract: {0}")]
    Invalid(String),
    #[error("game plan target collision: {0}")]
    TargetCollision(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GameInputFormat {
    Pak,
    Zip,
    DirectorySnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameSourceBinding {
    pub source_id: String,
    pub remote_game_key: String,
    pub provider_game_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameResourceRequirement {
    pub resource_id: String,
    pub version_range: String,
    pub optional: bool,
    pub purpose: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameVariantDescriptor {
    pub variant_id: String,
    pub display_name: String,
    pub store: String,
    pub store_app_id: Option<String>,
    pub target: String,
    pub main_pak_relative_path: String,
    pub paks_relative_path: String,
    pub binaries_relative_path: String,
    pub mod_pak_relative_path: String,
    pub process_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameDescriptor {
    pub schema_version: u32,
    pub game_id: String,
    pub display_name: String,
    pub adapter_version: String,
    pub settings_schema_version: u32,
    pub settings_schema: serde_json::Value,
    pub variants: Vec<GameVariantDescriptor>,
    pub input_formats: Vec<GameInputFormat>,
    pub source_bindings: Vec<GameSourceBinding>,
    pub resources: Vec<GameResourceRequirement>,
    pub capabilities: Vec<String>,
}

impl GameDescriptor {
    pub fn validate(&self) -> Result<(), GameContractError> {
        nonempty(&self.game_id, "game_id")?;
        nonempty(&self.display_name, "display_name")?;
        nonempty(&self.adapter_version, "adapter_version")?;
        if self.schema_version != 1 || self.settings_schema_version == 0 {
            return Err(GameContractError::Invalid(
                "descriptor and settings schema versions must be supported".to_owned(),
            ));
        }
        if self.variants.is_empty() || self.input_formats.is_empty() {
            return Err(GameContractError::Invalid(
                "descriptor requires variants and input formats".to_owned(),
            ));
        }
        let mut variants = BTreeSet::new();
        for variant in &self.variants {
            nonempty(&variant.variant_id, "variant_id")?;
            if !variants.insert(variant.variant_id.to_ascii_lowercase()) {
                return Err(GameContractError::Invalid(format!(
                    "duplicate game variant {}",
                    variant.variant_id
                )));
            }
            for path in [
                &variant.main_pak_relative_path,
                &variant.paks_relative_path,
                &variant.binaries_relative_path,
                &variant.mod_pak_relative_path,
            ] {
                checked_relative_path(path)?;
            }
            if variant.process_names.is_empty() {
                return Err(GameContractError::Invalid(format!(
                    "variant {} has no process detection names",
                    variant.variant_id
                )));
            }
        }
        let mut sources = BTreeSet::new();
        for source in &self.source_bindings {
            if !sources.insert(source.source_id.to_ascii_lowercase()) {
                return Err(GameContractError::Invalid(format!(
                    "duplicate source binding {}",
                    source.source_id
                )));
            }
            nonempty(&source.remote_game_key, "remote_game_key")?;
            nonempty(&source.provider_game_id, "provider_game_id")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoveryConfidence {
    UserSelected,
    StoreManifest,
    LayoutMatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameInstallationCandidate {
    pub game_id: String,
    pub variant_id: String,
    pub root_path: String,
    pub main_pak_path: String,
    pub confidence: DiscoveryConfidence,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidatedGameInstallation {
    pub game_id: String,
    pub variant_id: String,
    pub root_path: String,
    pub main_pak_relative_path: String,
    pub fingerprint: String,
    pub target: String,
    pub main_pak_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunningProcess {
    pub pid: u32,
    pub executable_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameSettingsSnapshot {
    pub game_id: String,
    pub schema_version: u32,
    pub revision: u64,
    pub options: serde_json::Value,
    pub digest: String,
}

impl GameSettingsSnapshot {
    pub fn new(
        game_id: impl Into<String>,
        schema_version: u32,
        revision: u64,
        options: serde_json::Value,
    ) -> Result<Self, GameContractError> {
        let game_id = game_id.into();
        nonempty(&game_id, "settings game_id")?;
        if schema_version == 0 {
            return Err(GameContractError::Invalid(
                "settings schema version must be positive".to_owned(),
            ));
        }
        let digest = canonical_digest(&SettingsDigestInput {
            game_id: &game_id,
            schema_version,
            revision,
            options: &options,
        })?;
        Ok(Self {
            game_id,
            schema_version,
            revision,
            options,
            digest,
        })
    }

    pub fn verify(&self) -> Result<(), GameContractError> {
        let expected = canonical_digest(&SettingsDigestInput {
            game_id: &self.game_id,
            schema_version: self.schema_version,
            revision: self.revision,
            options: &self.options,
        })?;
        if expected != self.digest {
            return Err(GameContractError::Invalid(
                "settings snapshot digest differs".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsDigestInput<'a> {
    game_id: &'a str,
    schema_version: u32,
    revision: u64,
    options: &'a serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlannedGameInput {
    pub input_id: String,
    pub display_name: String,
    pub source_id: String,
    pub remote_game_key: String,
    pub remote_mod_id: String,
    pub format: GameInputFormat,
    pub artifact: ArtifactRef,
    pub audio_only_hint: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum GameBuildStep {
    InspectInputs {
        input_ids: Vec<String>,
    },
    GamePatch {
        patch_contract_version: u32,
        base_game_artifact: ArtifactRef,
        resource_lock_digest: Option<String>,
    },
    UnrealPakBuild {
        input_ids: Vec<String>,
        content_prefix: String,
        version: u8,
        compression: String,
    },
    LoaderPlan {
        input_ids: Vec<String>,
        loader_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GameOutputRole {
    ModPak,
    AudioPak,
    HookDll,
    LoaderFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlannedGameOutput {
    pub role: GameOutputRole,
    pub target_relative_path: String,
    pub owner: String,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameBuildPlan {
    pub schema_version: u32,
    pub plan_hash: String,
    pub game_id: String,
    pub variant_id: String,
    pub game_fingerprint: String,
    pub profile_revision: u64,
    pub module_lock_digest: String,
    pub settings: GameSettingsSnapshot,
    pub ordered_inputs: Vec<PlannedGameInput>,
    pub steps: Vec<GameBuildStep>,
    pub outputs: Vec<PlannedGameOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuiltGameOutput {
    pub role: GameOutputRole,
    pub target_relative_path: String,
    pub artifact: ArtifactRef,
    pub owner: String,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeployPrecondition {
    Absent,
    Digest(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameDeployFile {
    pub target_relative_path: String,
    pub artifact: ArtifactRef,
    pub owner: String,
    pub executable: bool,
    pub precondition: DeployPrecondition,
    pub approval_kinds: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameDeployPlan {
    pub schema_version: u32,
    pub game_id: String,
    pub variant_id: String,
    pub game_fingerprint: String,
    pub profile_revision: u64,
    pub build_plan_hash: String,
    pub module_lock_digest: String,
    pub files: Vec<GameDeployFile>,
}

impl GameDeployPlan {
    pub fn validate(&self) -> Result<(), GameContractError> {
        if self.schema_version != GAME_PLAN_SCHEMA_VERSION {
            return Err(GameContractError::Invalid(
                "unsupported deploy plan schema".to_owned(),
            ));
        }
        let mut targets = BTreeSet::new();
        for file in &self.files {
            let target = checked_relative_path(&file.target_relative_path)?;
            if target != file.target_relative_path {
                return Err(GameContractError::Invalid(format!(
                    "deploy target is not normalized: {}",
                    file.target_relative_path
                )));
            }
            if !targets.insert(target.to_lowercase()) {
                return Err(GameContractError::TargetCollision(target));
            }
            if !file.artifact.verified {
                return Err(GameContractError::Invalid(format!(
                    "deploy artifact {} is not verified",
                    file.artifact.digest
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LegacyFileDisposition {
    TakeoverRequired,
    ForeignPreserved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyFileCandidate {
    pub relative_path: String,
    pub disposition: LegacyFileDisposition,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyInstallReport {
    pub game_id: String,
    pub variant_id: String,
    pub files: Vec<LegacyFileCandidate>,
    pub requires_approval: bool,
}

pub fn build_plan_hash(plan: &GameBuildPlan) -> Result<String, GameContractError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct HashablePlan<'a> {
        schema_version: u32,
        game_id: &'a str,
        variant_id: &'a str,
        game_fingerprint: &'a str,
        profile_revision: u64,
        module_lock_digest: &'a str,
        settings: &'a GameSettingsSnapshot,
        ordered_inputs: &'a [PlannedGameInput],
        steps: &'a [GameBuildStep],
        outputs: &'a [PlannedGameOutput],
    }
    canonical_digest(&HashablePlan {
        schema_version: plan.schema_version,
        game_id: &plan.game_id,
        variant_id: &plan.variant_id,
        game_fingerprint: &plan.game_fingerprint,
        profile_revision: plan.profile_revision,
        module_lock_digest: &plan.module_lock_digest,
        settings: &plan.settings,
        ordered_inputs: &plan.ordered_inputs,
        steps: &plan.steps,
        outputs: &plan.outputs,
    })
}

pub fn canonical_digest(value: &impl Serialize) -> Result<String, GameContractError> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(value)?)
    ))
}

pub fn group_preconditions(
    entries: impl IntoIterator<Item = (String, DeployPrecondition)>,
) -> Result<BTreeMap<String, DeployPrecondition>, GameContractError> {
    let mut result = BTreeMap::new();
    for (path, expected) in entries {
        let path = checked_relative_path(&path)?;
        let key = path.to_lowercase();
        if result.insert(key, expected).is_some() {
            return Err(GameContractError::TargetCollision(path));
        }
    }
    Ok(result)
}

fn nonempty(value: &str, field: &str) -> Result<(), GameContractError> {
    if value.trim().is_empty() || value.contains(['\0', '\n', '\r']) {
        Err(GameContractError::Invalid(format!(
            "{field} must be non-empty and single-line"
        )))
    } else {
        Ok(())
    }
}

fn checked_relative_path(value: &str) -> Result<String, GameContractError> {
    if value.is_empty()
        || value.len() > 4 * 1024
        || value.starts_with(['/', '\\'])
        || value.contains('\\')
        || value.contains(':')
        || value.contains('\0')
    {
        return Err(GameContractError::Invalid(format!(
            "path is not portable and relative: {value}"
        )));
    }
    let mut result = String::new();
    for component in value.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(GameContractError::Invalid(format!(
                "path contains an invalid component: {value}"
            )));
        }
        if !result.is_empty() {
            result.push('/');
        }
        result.push_str(component);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn settings_snapshots_bind_game_revision_and_options() {
        let drg = GameSettingsSnapshot::new(
            "game:drg",
            1,
            7,
            json!({"compression": "zlib", "ue4ss": true}),
        )
        .unwrap();
        let rc = GameSettingsSnapshot::new(
            "game:rogue-core",
            1,
            7,
            json!({"compression": "zlib", "ue4ss": true}),
        )
        .unwrap();
        assert_ne!(drg.digest, rc.digest);
        drg.verify().unwrap();
    }

    #[test]
    fn deploy_plan_rejects_case_colliding_targets() {
        let artifact = ArtifactRef {
            digest: format!("sha256:{:064x}", 1),
            size: 1,
            verified: true,
            media_type: None,
        };
        let plan = GameDeployPlan {
            schema_version: 1,
            game_id: "game:test".to_owned(),
            variant_id: "test".to_owned(),
            game_fingerprint: artifact.digest.clone(),
            profile_revision: 1,
            build_plan_hash: artifact.digest.clone(),
            module_lock_digest: artifact.digest.clone(),
            files: vec![
                GameDeployFile {
                    target_relative_path: "Content/a.pak".to_owned(),
                    artifact: artifact.clone(),
                    owner: "test".to_owned(),
                    executable: false,
                    precondition: DeployPrecondition::Absent,
                    approval_kinds: Vec::new(),
                },
                GameDeployFile {
                    target_relative_path: "content/A.pak".to_owned(),
                    artifact,
                    owner: "test".to_owned(),
                    executable: false,
                    precondition: DeployPrecondition::Absent,
                    approval_kinds: Vec::new(),
                },
            ],
        };
        assert!(matches!(
            plan.validate(),
            Err(GameContractError::TargetCollision(_))
        ));
    }
}
