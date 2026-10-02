use mintcat_engine_unreal_pak::UnrealEngineBackend;
use mintcat_game_api::{WorkerBuiltOutput, WorkerGameBuildRequest, WorkerGameBuildResponse};
use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore, ModuleLease};
use mintcat_sdk::WorkerRequest;

use crate::{DrgBuildInput, DrgBuildRequest, DrgBuilder, DrgError, DrgPatchPackage};

pub fn execute<E: UnrealEngineBackend>(
    request: &WorkerRequest,
    params: WorkerGameBuildRequest,
    builder: &DrgBuilder<E>,
    store: &ArtifactStore,
) -> Result<WorkerGameBuildResponse, DrgError> {
    let base = ArtifactGrant::from_worker_request(request, &params.base_grant_id)?;
    let inputs = params.inputs.iter().map(|input| {
        ArtifactGrant::from_worker_request(request, &input.grant_id)
    }).collect::<Result<Vec<_>, _>>()?;
    let patch = params.patch_package.map(ModuleLease::from_worker_grant).transpose()?;
    let patch_package = match (&patch, params.patch_archive_path) {
        (Some(package), Some(archive_path)) => Some(DrgPatchPackage { package, archive_path }),
        (None, None) => None,
        _ => return Err(DrgError::InvalidRequest("patch package and archive path must be supplied together".to_owned())),
    };
    let result = builder.build(DrgBuildRequest {
        plan: &params.plan,
        base_game_artifact: &base,
        inputs: params.inputs.iter().zip(&inputs).map(|(input, artifact)| DrgBuildInput {
            input_id: input.input_id.clone(), artifact,
        }).collect(),
        patch_package,
    })?;
    let outputs = result.outputs.into_iter().map(|output| {
        let grant = store.grant_blob(&output.artifact)?.descriptor.clone();
        Ok(WorkerBuiltOutput { output, grant })
    }).collect::<Result<Vec<_>, DrgError>>()?;
    Ok(WorkerGameBuildResponse { outputs })
}
