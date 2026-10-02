use std::collections::BTreeMap;

use mintcat_loader_ue4ss::DeploymentManifest;
use mintcat_runtime_artifacts::WorkerModuleGrant;
use mintcat_sdk::ArtifactGrantDescriptor;
use serde::{Deserialize, Serialize};

use crate::{BuiltGameOutput, DeployPrecondition, GameBuildPlan};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerInputGrant {
    pub input_id: String,
    pub grant_id: String,
    pub mod_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerGameDependency {
    pub contract_id: String,
    pub contract_version: u64,
    pub package_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerGameBuildRequest {
    pub plan: GameBuildPlan,
    pub dependencies: Vec<WorkerGameDependency>,
    pub base_grant_id: String,
    pub inputs: Vec<WorkerInputGrant>,
    pub patch_package: Option<WorkerModuleGrant>,
    pub patch_archive_path: Option<String>,
    pub resources: Vec<WorkerModuleGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerBuiltOutput {
    pub output: BuiltGameOutput,
    pub grant: ArtifactGrantDescriptor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerGameBuildResponse {
    pub outputs: Vec<WorkerBuiltOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerGameConflict {
    pub relative_path: String,
    pub winner_input_id: String,
    pub shadowed_input_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerGamePreparationResponse {
    pub conflicts: Vec<WorkerGameConflict>,
    pub companion_manifests: Vec<DeploymentManifest>,
    pub grants: Vec<ArtifactGrantDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerGameDeployRequest {
    pub build_plan: GameBuildPlan,
    pub built_outputs: Vec<BuiltGameOutput>,
    pub loader_manifests: Vec<DeploymentManifest>,
    pub preconditions: BTreeMap<String, DeployPrecondition>,
}
