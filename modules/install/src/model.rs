use std::collections::BTreeSet;

use mintcat_game_api::{
    BuiltGameOutput, GameBuildPlan, GameDeployPlan, GameInputFormat, GameSettingsSnapshot,
    LegacyInstallReport, PlannedGameInput, ValidatedGameInstallation,
};
use mintcat_runtime_artifacts::ArtifactGrant;
use mintcat_runtime_file_transactions::InstallReceipt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContentInputMetadata {
    pub profile_mod_id: String,
    pub format: GameInputFormat,
    pub audio_only_hint: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileInputSnapshot {
    pub profile_mod_id: String,
    pub mod_id: String,
    pub version_id: String,
    pub sort_order: u64,
    pub input: PlannedGameInput,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileBuildSnapshot {
    pub schema_version: u32,
    pub profile_id: String,
    pub profile_incarnation_id: String,
    pub game_id: String,
    pub installation_id: String,
    pub installation_incarnation_id: String,
    pub installation_state: String,
    pub root_path: String,
    pub profile_revision: u64,
    pub content_revision: u64,
    pub ordered_inputs: Vec<ProfileInputSnapshot>,
    pub order_warnings: Vec<String>,
    pub digest: String,
}

impl ProfileBuildSnapshot {
    pub fn planned_inputs(&self) -> Vec<PlannedGameInput> {
        self.ordered_inputs
            .iter()
            .map(|input| input.input.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleClosureEntry {
    pub module_id: String,
    pub version: String,
    pub package_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleClosure {
    pub schema_version: u32,
    pub entries: Vec<ModuleClosureEntry>,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildConflict {
    pub relative_path: String,
    pub winner_input_id: String,
    pub shadowed_input_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WinnerSummary {
    pub relative_path: String,
    pub winner_input_id: String,
    pub winner_display_name: String,
    pub shadowed_input_ids: Vec<String>,
    pub shadowed_display_names: Vec<String>,
    pub rule: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildSummary {
    pub ordered_input_ids: Vec<String>,
    pub winners: Vec<WinnerSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPurpose {
    Pack,
    Install,
}

pub struct AdapterBuildRequest<'a> {
    pub purpose: BuildPurpose,
    pub installation: &'a ValidatedGameInstallation,
    pub profile: &'a ProfileBuildSnapshot,
    pub settings: &'a GameSettingsSnapshot,
    pub module_closure: &'a ModuleClosure,
    pub input_grants: &'a [ArtifactGrant],
    pub cancellation: &'a mintcat_runtime_file_transactions::FileCancellation,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdapterBuildProduct {
    pub build_plan: GameBuildPlan,
    pub outputs: Vec<BuiltGameOutput>,
    pub deploy_plan: Option<GameDeployPlan>,
    pub conflicts: Vec<BuildConflict>,
    pub companion_manifests: Vec<mintcat_loader_ue4ss::DeploymentManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildRequest {
    pub owner_scope: String,
    pub profile_id: String,
    pub expected_profile_revision: u64,
    pub expected_content_revision: u64,
    pub installation_id: String,
    pub variant_id: String,
    pub settings: GameSettingsSnapshot,
    pub module_closure: ModuleClosure,
    pub input_metadata: Vec<ContentInputMetadata>,
}

#[derive(Debug, Clone)]
pub struct PlanInstallRequest {
    pub build: BuildRequest,
    pub expires_in_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackResult {
    pub profile: ProfileBuildSnapshot,
    pub module_closure: ModuleClosure,
    pub build_plan: GameBuildPlan,
    pub outputs: Vec<BuiltGameOutput>,
    pub companion_manifests: Vec<mintcat_loader_ue4ss::DeploymentManifest>,
    pub summary: BuildSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImmutableInstallPlan {
    pub schema_version: u32,
    pub plan_id: String,
    pub plan_hash: String,
    pub owner_scope: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub variant_id: String,
    pub profile: ProfileBuildSnapshot,
    pub input_metadata: Vec<ContentInputMetadata>,
    pub module_closure: ModuleClosure,
    pub settings: GameSettingsSnapshot,
    pub build_plan: GameBuildPlan,
    pub deploy_plan: GameDeployPlan,
    pub legacy_install: LegacyInstallReport,
    pub summary: BuildSummary,
    pub required_approvals: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalGrant {
    pub approval_ref: String,
    pub plan_id: String,
    pub plan_hash: String,
    pub approved_risks: BTreeSet<String>,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ApplyDisposition {
    Committed,
    Cancelled,
    AlreadyCommitted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyResult {
    pub plan_id: String,
    pub plan_hash: String,
    pub disposition: ApplyDisposition,
    pub receipt: Option<InstallReceipt>,
    pub cancel_requested_during_commit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UninstallRequest {
    pub installation_id: String,
    pub receipt_id: String,
    pub expected_profile_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UnifiedUninstallDisposition {
    Cancelled,
    Completed,
    CompletedWithPreservedFiles,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnifiedUninstallResult {
    pub installation_id: String,
    pub requested_receipt_id: String,
    pub disposition: UnifiedUninstallDisposition,
    pub processed_receipt_ids: Vec<String>,
    pub restored: Vec<String>,
    pub removed: Vec<String>,
    pub preserved_modified: Vec<String>,
    pub missing: Vec<String>,
    pub foreign_files_untouched: bool,
    pub cancel_requested_during_commit: bool,
}
