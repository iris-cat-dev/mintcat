use std::path::PathBuf;

use mintcat_engine_unreal_pak::{game_inputs, RemoteUnrealPakEngine};
use mintcat_game_api::{
    RunningProcess, WorkerGameBuildRequest, WorkerGameDeployRequest, GAME_ADAPTER_CONTRACT_ID,
    GAME_ADAPTER_CONTRACT_VERSION,
};
use mintcat_game_rogue_core::{
    create_build_plan, create_deploy_plan, descriptor, detect_legacy_install, ensure_not_running,
    validate_installation, RogueCoreBuildLimits, RogueCoreBuildPlanRequest, RogueCoreBuilder,
    RogueCoreError, RogueCoreSettings, RogueCoreVariant,
};
use mintcat_runtime_artifacts::{ArtifactStore, TrustStore};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_sdk::{
    run_worker_with_broker, worker_app_error, AppError, WorkerBroker, WorkerIdentity, WorkerRequest,
    WORKER_WORKSPACE_ENV,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let workspace = PathBuf::from(std::env::var_os(WORKER_WORKSPACE_ENV)
        .ok_or_else(|| anyhow::anyhow!("{WORKER_WORKSPACE_ENV} is required"))?);
    let metadata = std::fs::symlink_metadata(&workspace)?;
    anyhow::ensure!(workspace.is_absolute() && metadata.is_dir()
        && !metadata.file_type().is_symlink(), "worker workspace must be an absolute real directory");
    let workspace = std::fs::canonicalize(workspace)?;
    let layout = RuntimePaths::new(workspace.join("state"))?
        .initialize(RuntimeEnvironment::Production)?;
    let store = ArtifactStore::from_layout(
        &layout, RuntimeEnvironment::Production, "worker-private", "1", TrustStore::default(),
    )?;
    let staging = workspace.join("build");
    run_worker_with_broker(WorkerIdentity::from_env()?, move |request, broker| {
        let store = store.clone();
        let staging = staging.clone();
        async move { handle(request, broker, staging, store).await }
    }).await?;
    Ok(())
}

async fn handle(
    request: WorkerRequest,
    broker: WorkerBroker,
    staging: PathBuf,
    store: ArtifactStore,
) -> Result<Value, Box<AppError>> {
    if request.contract_id.as_deref() != Some(GAME_ADAPTER_CONTRACT_ID)
        || request.contract_version != Some(GAME_ADAPTER_CONTRACT_VERSION)
    {
        return Err(worker_app_error(
            &request.id,
            "INCOMPATIBLE_CONTRACT",
            "game.contract.incompatible",
            json!({
                "expectedId": GAME_ADAPTER_CONTRACT_ID,
                "expectedVersion": GAME_ADAPTER_CONTRACT_VERSION
            }),
        ));
    }
    let request_id = request.id.clone();
    let outcome = match request.method.as_deref().unwrap_or_default() {
        "describe" => serde_json::to_value(descriptor()).map_err(RogueCoreError::from),
        "settings.snapshot" => settings_snapshot(request.params),
        "installation.validate" => validate(request.params),
        "installation.discover" => discover(request.params),
        "build.plan" => serde_json::from_value::<RogueCoreBuildPlanRequest>(request.params)
            .map_err(RogueCoreError::from)
            .and_then(create_build_plan)
            .and_then(|plan| serde_json::to_value(plan).map_err(RogueCoreError::from)),
        "build.prepare" | "build.execute" => tokio::task::spawn_blocking(move || {
            let mut request = request;
            let params: WorkerGameBuildRequest =
                serde_json::from_value(std::mem::take(&mut request.params))?;
            let engine_package_digest = game_inputs::selected_provider(&params.dependencies)?.to_owned();
            let engine = RemoteUnrealPakEngine::new(
                broker, engine_package_digest.clone(), store.clone(),
            );
            if request.method.as_deref() == Some("build.prepare") {
                return game_inputs::prepare(
                    &request, params, &descriptor(),
                    mintcat_loader_ue4ss::Ue4ssSettings::rogue_core(), &engine, &store,
                ).and_then(|result| serde_json::to_value(result).map_err(Into::into))
                    .map_err(RogueCoreError::from);
            }
            let builder = RogueCoreBuilder::open_with_engine(
                store.clone(), staging, RogueCoreBuildLimits::default(), engine,
            )?;
            let result = mintcat_game_rogue_core::worker_execution::execute(&request, params, &builder, &store)?;
            for built in &result.outputs {
                if matches!(built.output.role,
                    mintcat_game_api::GameOutputRole::ModPak | mintcat_game_api::GameOutputRole::AudioPak)
                {
                    eprintln!("engine.unreal-pak provider={} build-artifact={}",
                        engine_package_digest, built.output.artifact.digest);
                }
            }
            serde_json::to_value(result).map_err(RogueCoreError::from)
        }).await.map_err(|error| worker_app_error(
            &request_id, "BUILD_WORKER_FAILED", "game.build.worker_failed",
            json!({"detail": error.to_string()}),
        ))?,
        "deploy.plan" => serde_json::from_value::<WorkerGameDeployRequest>(request.params)
            .map_err(RogueCoreError::from)
            .and_then(|params| create_deploy_plan(
                &params.build_plan, params.built_outputs, params.loader_manifests,
                params.preconditions,
            ))
            .and_then(|plan| serde_json::to_value(plan).map_err(RogueCoreError::from)),
        "legacy.detect" => legacy_detect(request.params),
        method => {
            return Err(worker_app_error(
                &request.id,
                "UNKNOWN_METHOD",
                "game.method.unknown",
                json!({"method": method}),
            ));
        }
    };
    outcome.map_err(|error| map_error(&request_id, error))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsParams {
    revision: u64,
    options: RogueCoreSettings,
}

fn settings_snapshot(params: Value) -> Result<Value, RogueCoreError> {
    let params: SettingsParams = serde_json::from_value(params)?;
    Ok(serde_json::to_value(
        params.options.snapshot(params.revision)?,
    )?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ValidateParams {
    root_path: PathBuf,
    variant_id: String,
    #[serde(default)]
    running_processes: Vec<RunningProcess>,
}

fn validate(params: Value) -> Result<Value, RogueCoreError> {
    let params: ValidateParams = serde_json::from_value(params)?;
    let variant = RogueCoreVariant::from_id(&params.variant_id)?;
    ensure_not_running(variant, &params.running_processes)?;
    Ok(serde_json::to_value(validate_installation(
        &params.root_path,
        variant,
    )?)?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscoverParams {
    #[serde(default)]
    roots: Vec<DiscoverRoot>,
    #[serde(default)]
    include_steam: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscoverRoot { root_path: PathBuf, variant_id: String }

fn discover(params: Value) -> Result<Value, RogueCoreError> {
    let params: DiscoverParams = serde_json::from_value(params)?;
    if params.roots.len() > 32 {
        return Err(RogueCoreError::InvalidRequest("at most 32 trusted discovery hints are permitted".into()));
    }
    let mut candidates = Vec::new();
    if params.include_steam { candidates.extend(mintcat_game_rogue_core::discover_steam()?); }
    for hint in params.roots {
        let variant = RogueCoreVariant::from_id(&hint.variant_id)?;
        if !hint.root_path.is_absolute() || hint.root_path.as_os_str().len() > 4096 {
            return Err(RogueCoreError::InvalidRequest("discovery hint must be a bounded absolute root".into()));
        }
        candidates.extend(mintcat_game_rogue_core::discover_roots([
            (hint.root_path, variant, mintcat_game_api::DiscoveryConfidence::UserSelected),
        ])?);
    }
    candidates.sort_by(|a, b| (&a.root_path, &a.variant_id).cmp(&(&b.root_path, &b.variant_id)));
    candidates.dedup_by(|a, b| a.root_path == b.root_path && a.variant_id == b.variant_id);
    Ok(serde_json::to_value(candidates)?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyParams {
    root_path: PathBuf,
    variant_id: String,
}

fn legacy_detect(params: Value) -> Result<Value, RogueCoreError> {
    let params: LegacyParams = serde_json::from_value(params)?;
    Ok(serde_json::to_value(detect_legacy_install(
        &params.root_path,
        RogueCoreVariant::from_id(&params.variant_id)?,
    )?)?)
}

fn map_error(request_id: &str, error: RogueCoreError) -> Box<AppError> {
    let code = match error {
        RogueCoreError::InvalidRequest(_) | RogueCoreError::InvalidInput { .. } => {
            "INVALID_ARGUMENT"
        }
        RogueCoreError::UnsupportedLayout(_) | RogueCoreError::UnsupportedVersion(_) => {
            "UNSUPPORTED_GAME"
        }
        RogueCoreError::GameRunning(_) => "TARGET_RUNNING",
        RogueCoreError::Artifact(_) => "ARTIFACT_UNAVAILABLE",
        RogueCoreError::Contract(_)
        | RogueCoreError::Engine(_)
        | RogueCoreError::Pak(_)
        | RogueCoreError::Json(_)
        | RogueCoreError::Io { .. }
        | RogueCoreError::Archive { .. }
        | RogueCoreError::CompatibilityBuild(_)
        | RogueCoreError::Verification(_) => "GAME_ADAPTER_FAILED",
    };
    worker_app_error(
        request_id,
        code,
        "game.rogue_core.request_failed",
        json!({"message": error.to_string()}),
    )
}
