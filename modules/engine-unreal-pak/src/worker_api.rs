//! Engine contract parameters contain grant IDs, never caller-selected paths.
use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore};
use mintcat_sdk::{ArtifactGrantDescriptor, WorkerRequest};
use mintcat_unreal_pak::PakBuildTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    UnrealBuildManifest, UnrealBuildRequest, UnrealBuildResult, UnrealEngineError, UnrealInput,
    UnrealInputFormat, UnrealPakEngine, ENGINE_CONTRACT_ID, ENGINE_CONTRACT_VERSION,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerEngineInput {
    pub input_id: String,
    pub grant_id: String,
    pub format: UnrealInputFormat,
    pub content_prefix: Option<String>,
}

impl WorkerEngineInput {
    pub fn from_input(input: &UnrealInput<'_>) -> Self {
        Self {
            input_id: input.input_id.clone(),
            grant_id: input.artifact.descriptor.grant_id.clone(),
            format: input.format,
            content_prefix: input.content_prefix.clone(),
        }
    }

    fn borrowed<'a>(&self, artifact: &'a ArtifactGrant) -> UnrealInput<'a> {
        UnrealInput {
            input_id: self.input_id.clone(),
            artifact,
            format: self.format,
            content_prefix: self.content_prefix.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerEngineConflictsRequest {
    pub inputs: Vec<WorkerEngineInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerEngineBuildRequest {
    pub plan_hash: String,
    pub inputs: Vec<WorkerEngineInput>,
    pub reference_paths: Vec<String>,
    pub target: PakBuildTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerEngineBuildResponse {
    pub result: UnrealBuildResult,
    pub grant: ArtifactGrantDescriptor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerEngineVerifyRequest {
    pub grant_id: String,
    pub manifest: UnrealBuildManifest,
}

/// Execute only with grants from the supervisor-authenticated request envelope.
/// This synchronous entry point must run on the worker's blocking pool.
pub fn execute(
    request: &WorkerRequest,
    engine: &UnrealPakEngine,
    artifacts: &ArtifactStore,
) -> Result<Value, UnrealEngineError> {
    if request.contract_id.as_deref() != Some(ENGINE_CONTRACT_ID)
        || request.contract_version != Some(ENGINE_CONTRACT_VERSION)
    {
        return Err(UnrealEngineError::InvalidRequest(
            "incompatible engine contract".to_owned(),
        ));
    }
    match request.method.as_deref().unwrap_or_default() {
        "inspect" => {
            let params: WorkerEngineInput = serde_json::from_value(request.params.clone())?;
            let grant = ArtifactGrant::from_worker_request(request, &params.grant_id)?;
            Ok(serde_json::to_value(engine.inspect(params.borrowed(&grant))?)?)
        }
        "conflicts" => {
            let params: WorkerEngineConflictsRequest =
                serde_json::from_value(request.params.clone())?;
            let grants = resolve_inputs(request, &params.inputs)?;
            let inputs = params.inputs.iter().zip(&grants)
                .map(|(input, grant)| input.borrowed(grant)).collect();
            Ok(serde_json::to_value(engine.conflicts(inputs)?)?)
        }
        "build" => {
            let params: WorkerEngineBuildRequest = serde_json::from_value(request.params.clone())?;
            let grants = resolve_inputs(request, &params.inputs)?;
            let inputs = params.inputs.iter().zip(&grants)
                .map(|(input, grant)| input.borrowed(grant)).collect();
            let result = engine.build(UnrealBuildRequest {
                plan_hash: params.plan_hash,
                inputs,
                reference_paths: params.reference_paths,
                target: params.target,
            })?;
            let grant = artifacts.grant_blob(&result.artifact)?.descriptor.clone();
            Ok(serde_json::to_value(WorkerEngineBuildResponse { result, grant })?)
        }
        "verify" => {
            let params: WorkerEngineVerifyRequest = serde_json::from_value(request.params.clone())?;
            let grant = ArtifactGrant::from_worker_request(request, &params.grant_id)?;
            Ok(serde_json::to_value(engine.verify(&grant, &params.manifest)?)?)
        }
        method => Err(UnrealEngineError::InvalidRequest(format!(
            "unknown engine method: {method}"
        ))),
    }
}

fn resolve_inputs(
    request: &WorkerRequest,
    inputs: &[WorkerEngineInput],
) -> Result<Vec<ArtifactGrant>, UnrealEngineError> {
    inputs.iter().map(|input| {
        ArtifactGrant::from_worker_request(request, &input.grant_id)
            .map_err(UnrealEngineError::from)
    }).collect()
}
