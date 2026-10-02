use std::{fs, path::PathBuf};

use mintcat_engine_unreal_pak::{worker_api, UnrealEngineLimits, UnrealPakEngine};
use mintcat_runtime_artifacts::{ArtifactStore, TrustStore};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{
    run_worker_with_broker, worker_app_error, WorkerIdentity, WORKER_WORKSPACE_ENV,
};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let workspace = PathBuf::from(std::env::var_os(WORKER_WORKSPACE_ENV)
        .ok_or_else(|| anyhow::anyhow!("required worker environment variable {WORKER_WORKSPACE_ENV} is absent"))?);
    let metadata = fs::symlink_metadata(&workspace)?;
    anyhow::ensure!(workspace.is_absolute() && metadata.is_dir()
        && !metadata.file_type().is_symlink(), "worker workspace must be an absolute real directory");
    let workspace = fs::canonicalize(workspace)?;
    // These directories outlive the request and process shutdown. The host owns
    // their cleanup, so returned output grants remain readable for publication.
    let layout = RuntimePaths::new(workspace.join("state"))?
        .initialize(RuntimeEnvironment::Production)?;
    let artifacts = ArtifactStore::from_layout(
        &layout, RuntimeEnvironment::Production, "worker-private", "1", TrustStore::default(),
    )?;
    let engine = UnrealPakEngine::open(
        artifacts.clone(), workspace.join("build"), UnrealEngineLimits::default(),
    )?;
    run_worker_with_broker(WorkerIdentity::from_env()?, move |request, _broker| {
        let engine = engine.clone();
        let artifacts = artifacts.clone();
        async move {
            let request_id = request.id.clone();
            tokio::task::spawn_blocking(move || worker_api::execute(&request, &engine, &artifacts))
                .await
                .map_err(|error| worker_app_error(
                    &request_id, "ENGINE_WORKER_FAILED", "engine.unreal-pak.request_failed",
                    json!({"message": error.to_string()}),
                ))?
                .map_err(|error| worker_app_error(
                    &request_id, "ENGINE_REQUEST_FAILED", "engine.unreal-pak.request_failed",
                    json!({"message": error.to_string()}),
                ))
        }
    }).await?;
    Ok(())
}
