mod agent_proxy;
mod backup;
mod composition;
mod application;
mod sources;

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use mintcat_library_service::{
    DownloadOperationState, LibraryAuthorization, LibraryError, LibraryStore,
    CONTENT_WRITE_PERMISSION, METADATA_WRITE_PERMISSION, PROFILE_WRITE_PERMISSION, READ_PERMISSION,
};
use mintcat_runtime_artifacts::{ArtifactError, ArtifactStore, TrustStore};
use mintcat_runtime_broker::{
    BrokerError, CredentialStore, CredentialVault, MemoryCredentialStore, NetworkBroker,
    OsCredentialStore,
};
use mintcat_runtime_downloads::{DownloadCatalog, DownloadCoordinator, DownloadCoordinatorError};
use mintcat_runtime_ipc::{IpcError, IpcServer, ServerReady};
use mintcat_runtime_jobs::{
    request_hash, ConfirmationPolicy, JobError, JobStore, RegisterJob,
};
use mintcat_runtime_kernel::{
    KernelDatabase, KernelDatabaseError, RuntimeEnvironment, RuntimeLayout, RuntimePathError,
    RuntimePaths,
};
use mintcat_runtime_supervisor::{ModuleSupervisor, SupervisorError};
use mintcat_runtime_transfers::{
    TransferConfig, TransferError, TransferManager, TransferState as RuntimeTransferState,
};
use mintcat_feature_backup::{CloudBackupTransport, CloudCredentialProvider, NativeBackupCodec};
use mintcat_install::{GameAdapterRegistry, GameCatalog, InstallCoordinator, InstallService};
use mintcat_runtime_broker::{InteractionClient, InteractionHost};
use mintcat_runtime_file_transactions::FileTransactionManager;
use mintcat_runtime_updates::UpdatesCoordinator;
use mintcat_source_service::{SourceApplication, SourceNetworkHost};
use mintcat_sdk::{
    ActionRequired, AppError, ApplicationCommand, DoctorCheck, DoctorCheckStatus, DownloadMode,
    DownloadOverlay, DownloadOverlayItem, JobState, JsonRpcId, ProfilesContentStateResult,
    RecoveryAction, SystemDoctorResult, SystemStatusResult, TransferState as ContractTransferState,
    ValidatedRequest,
};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error(transparent)]
    RuntimePath(#[from] RuntimePathError),
    #[error(transparent)]
    Ipc(#[from] IpcError),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error(transparent)]
    KernelDatabase(#[from] KernelDatabaseError),
    #[error(transparent)]
    Job(#[from] JobError),
    #[error(transparent)]
    Library(#[from] LibraryError),
    #[error(transparent)]
    Broker(#[from] BrokerError),
    #[error(transparent)]
    Transfer(#[from] TransferError),
    #[error(transparent)]
    Download(#[from] DownloadCoordinatorError),
    #[error("runtime composition: {0}")]
    Composition(String),
}

pub struct BoundDaemon {
    server: IpcServer,
    state: Arc<DaemonState>,
}

struct DaemonState {
    layout: RuntimeLayout,
    environment: RuntimeEnvironment,
    generation_id: String,
    supervisor: ModuleSupervisor,
    jobs: JobStore,
    library: LibraryStore,
    library_authorization: LibraryAuthorization,
    downloads: DownloadCoordinator,
    transfers: TransferManager,
    artifacts: ArtifactStore,
    network_broker: NetworkBroker,
    credential_vault: CredentialVault,
    interactions: InteractionClient,
    interaction_host: InteractionHost,
    source_network: Arc<SourceNetworkHost>,
    composition_lock: tokio::sync::Mutex<()>,
    sources: RwLock<Option<SourceGeneration>>,
    source_generations: RwLock<BTreeMap<String, SourceGeneration>>,
    updates: Option<UpdatesCoordinator>,
    configuration: Option<composition::RuntimeConfiguration>,
    install: InstallCoordinator,
    features: Arc<agent_proxy::FeatureRuntime>,
    backup_codec: RwLock<Option<Arc<NativeBackupCodec>>>,
    backup_pending_codecs: Mutex<HashMap<String, Arc<NativeBackupCodec>>>,
    backup_running: Mutex<std::collections::HashSet<String>>,
    backup_cloud: Option<Arc<CloudBackupTransport>>,
    backup_cloud_credentials: Option<Arc<dyn CloudCredentialProvider>>,
    bootstrap_error: parking_lot::Mutex<Option<String>>,
    backup_startup_blocked: std::sync::atomic::AtomicBool,
    recovered: Vec<mintcat_runtime_jobs::RecoveryRecord>,
}

#[derive(Clone)]
struct SourceGeneration {
    environment_generation: String,
    scope_confirmations: Arc<[bool]>,
    lock_digest: String,
    application: SourceApplication,
    downloads: DownloadCoordinator,
    supervisor: ModuleSupervisor,
    lease_id: Option<String>,
}


#[derive(Clone, Copy, PartialEq, Eq)]
enum Principal { Human, Agent }

fn unix_seconds() -> Result<u64, std::io::Error> {
    Ok(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?.as_secs())
}

fn now_ms() -> Result<i64, std::io::Error> {
    let elapsed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?;
    i64::try_from(elapsed.as_millis()).map_err(std::io::Error::other)
}

impl BoundDaemon {
    pub fn bind(
        paths: &RuntimePaths,
        environment: RuntimeEnvironment,
    ) -> Result<Self, DaemonError> {
        let layout = paths.initialize(environment)?;
        let server = IpcServer::bind(paths)?;
        let generation_id = server.ready().generation_id;
        let database = KernelDatabase::open(&layout)?;
        let jobs = JobStore::open(database.clone(), &generation_id, 256)?;
        let (configuration, mut configuration_error) = match composition::RuntimeConfiguration::load(&layout) {
            Ok(configuration) => (configuration, None),
            Err(error) => (None, Some(error)),
        };
        let trust = match configuration.as_ref().map(composition::RuntimeConfiguration::trust).transpose() {
            Ok(trust) => trust.unwrap_or_default(),
            Err(error) => {
                configuration_error = Some(error);
                TrustStore::default()
            }
        };
        let artifacts = ArtifactStore::from_layout(
            &layout, environment, target_lexicon::HOST.to_string(), "1", trust,
        )?;
        let library = LibraryStore::unavailable(&layout);
        #[cfg(any(test, feature = "fixtures"))]
        let library = if environment == RuntimeEnvironment::Test && configuration.is_none() && configuration_error.is_none() {
            LibraryStore::open(&layout)?
        } else { library };
        let library_authorization = LibraryAuthorization::new(
            LOCAL_OWNER_SCOPE,
            [READ_PERMISSION, PROFILE_WRITE_PERMISSION, CONTENT_WRITE_PERMISSION, METADATA_WRITE_PERMISSION],
        );
        // Kernel recovery does not start any Library writer or touch game files.
        let recovered = jobs.recover_incomplete()?;
        let credential_store: Arc<dyn CredentialStore> = if environment == RuntimeEnvironment::Test {
            Arc::new(MemoryCredentialStore::default())
        } else {
            Arc::new(OsCredentialStore::new("mintcat-vnext")?)
        };
        let credential_vault = CredentialVault::new(credential_store);
        let network_broker = NetworkBroker::new(&credential_vault, None)?;
        let transfers = TransferManager::open(
            database.clone(), artifacts.clone(), network_broker.clone(),
            layout.runtime_dir.join("transfers"), TransferConfig::default(),
        )?;
        let downloads = DownloadCoordinator::new(
            library.clone(), library_authorization.clone(), jobs.clone(), transfers.clone(),
            artifacts.clone(), DownloadCatalog::default(),
        );
        let supervisor = ModuleSupervisor::new(artifacts.clone(), &generation_id, &layout.runtime_dir)?;
        let (interactions, interaction_host) = mintcat_runtime_broker::open_interaction_broker(database.clone(), jobs.clone())?;
        let source_network = Arc::new(SourceNetworkHost::new(network_broker.clone(), Vec::new())?);
        let adapters = GameAdapterRegistry::default();
        let catalog = GameCatalog::new(supervisor.clone(), adapters.clone());
        let transactions = FileTransactionManager::open(database.clone(), layout.state_dir.join("file-transactions"))
            .map_err(|error| DaemonError::Composition(error.to_string()))?;
        let service = InstallService::open(
            database.clone(), library.clone(), LOCAL_OWNER_SCOPE, &generation_id,
            artifacts.clone(), transactions, adapters,
        ).map_err(|error| DaemonError::Composition(error.to_string()))?;
        let install = InstallCoordinator::new(service, jobs.clone(), catalog, layout.clone(), LOCAL_OWNER_SCOPE);
        let features = Arc::new(agent_proxy::FeatureRuntime::new(&layout, artifacts.clone(), jobs.clone(), network_broker.clone())
            .map_err(|error| DaemonError::Composition(format!("{}: {}", error.code, error.message_key)))?);
        let updates = match configuration.as_ref().map(|configuration| configuration.updates(&layout, database, artifacts.clone())).transpose() {
            Ok(updates) => updates,
            Err(error) => {
                configuration_error = Some(error);
                None
            }
        };
        let (backup_cloud, backup_cloud_credentials) = match configuration.as_ref().map(|configuration| configuration.cloud(network_broker.clone())).transpose() {
            Ok(cloud) => cloud.flatten().map(|(transport, credentials)| (Some(transport), Some(credentials))).unwrap_or_default(),
            Err(error) => {
                configuration_error = Some(error);
                (None, None)
            }
        };
        let state = Arc::new(DaemonState {
            layout, environment, generation_id, supervisor, jobs, library, library_authorization,
            downloads, transfers, artifacts, network_broker, credential_vault,
            interactions, interaction_host, source_network, sources: RwLock::new(None),
            source_generations: RwLock::new(BTreeMap::new()),
            composition_lock: tokio::sync::Mutex::new(()),
            updates, configuration, install, features,
            backup_codec: RwLock::new(None), backup_pending_codecs: Mutex::new(HashMap::new()),
            backup_running: Mutex::new(std::collections::HashSet::new()),
            backup_cloud, backup_cloud_credentials, bootstrap_error: parking_lot::Mutex::new(configuration_error),
            backup_startup_blocked: std::sync::atomic::AtomicBool::new(false), recovered,
        });
        state.features.bind_state(&state).map_err(|error| DaemonError::Composition(format!("{}: {}", error.code, error.message_key)))?;
        composition::register_owners(&state).map_err(DaemonError::Composition)?;
        Ok(Self { server, state })
    }

    pub fn ready(&self) -> ServerReady {
        self.server.ready()
    }

    pub fn job_store(&self) -> &JobStore {
        &self.state.jobs
    }
    pub fn library_store(&self) -> &LibraryStore {
        &self.state.library
    }


    pub fn replay_library_outbox(&self) -> Result<usize, DaemonError> {
        replay_library_outbox(&self.state.library, &self.state.jobs)
    }

    pub async fn run<Shutdown>(self, shutdown: Shutdown) -> Result<(), DaemonError>
    where
        Shutdown: Future<Output = ()> + Send,
    {
        self.state.features.bind_executor().map_err(|error| DaemonError::Composition(format!("{}: {}", error.code, error.message_key)))?;
        let (ready, waiting) = tokio::sync::watch::channel(false);
        let startup_state = Arc::clone(&self.state);
        let initialize = tokio::spawn(async move {
            let state = startup_state;
            let result: Result<(), String> = async {
                if state.bootstrap_error.lock().is_some() { return Ok(()); }
                let fixture = cfg!(any(test, feature = "fixtures")) && state.environment == RuntimeEnvironment::Test && state.configuration.is_none();
                if fixture {
                    let state = state.clone();
                    tokio::task::spawn_blocking(move || {
                        state.library.recover_download_operations(&state.generation_id).map_err(|error| error.to_string())?;
                        replay_library_outbox(&state.library, &state.jobs).map_err(|error| error.to_string())?;
                        Ok(())
                    }).await.map_err(|error| error.to_string()).and_then(|result: Result<(), String>| result)?;
                } else {
                    composition::bootstrap(&state).await?;
                }
                state.features.resume_recovered().await.map_err(|error| format!("{}: {}", error.code, error.message_key))?;
                composition::resume_downloads(&state)?;
                Ok(())
            }.await;
            if let Err(error) = result {
                eprintln!("mintcatd readonly rescue: {error}");
                *state.bootstrap_error.lock() = Some(error);
            }
            ready.send_replace(true);
        });
        // Handshake immediately; business requests wait for exact signed startup or rescue.
        // A large baseline must not consume the client's shorter handshake deadline.
        let state = Arc::clone(&self.state);
        let serve = self.server.run(
            Arc::new(move |request| {
                let state = Arc::clone(&state);
                let mut waiting = waiting.clone();
                async move {
                    let _ = waiting.wait_for(|ready| *ready).await;
                    application::handle_request(state, request, Principal::Human).await
                }
            }),
            shutdown,
        );
        let (startup_result, server_result) = tokio::join!(initialize, serve);
        let feature_result = self.state.features.shutdown().await
            .map_err(|error| DaemonError::Composition(format!("{}: {}", error.code, error.message_key)));
        let supervisor_result = self.state.supervisor.shutdown_all().await;
        let source_result = sources::shutdown(&self.state).await.map_err(DaemonError::Composition);
        let update_result = match &self.state.updates {
            Some(updates) => updates.shutdown().await.map_err(|error| DaemonError::Composition(error.to_string())),
            None => Ok(()),
        };
        startup_result.map_err(|error| DaemonError::Composition(format!("startup task failed: {error}")))?;
        server_result?;
        feature_result?;
        supervisor_result?;
        source_result?;
        update_result?;
        Ok(())
    }
}

fn spawn_download(
    downloads: DownloadCoordinator,
    job_id: String,
    operation_id: String,
    action: Option<ActionRequired>,
) {
    tokio::spawn(async move {
        if let Err(error) = downloads
            .execute(&job_id, &operation_id, action.as_ref())
            .await
        {
            eprintln!(
                "mintcatd download operation {operation_id} failed for job {job_id}: {error}"
            );
        }
    });
}

const LOCAL_OWNER_SCOPE: &str = "local-user";

fn replay_library_outbox(library: &LibraryStore, jobs: &JobStore) -> Result<usize, DaemonError> {
    let mut delivered = 0usize;
    loop {
        let mut events = library.pending_outbox("kernel-job", 256)?;
        events.extend(library.pending_outbox("kernel-job-control", 256)?);
        if events.is_empty() {
            return Ok(delivered);
        }
        events.sort_by_key(|event| event.event_seq);
        for event in events {
            let operation_id = event
                .operation_id
                .as_deref()
                .or_else(|| event.payload.get("operationId").and_then(Value::as_str))
                .ok_or_else(|| {
                    LibraryError::InvalidRecord("outbox operation id is missing".to_owned())
                })?;
            let owner_scope = event
                .payload
                .get("ownerScope")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    LibraryError::InvalidRecord("outbox owner scope is missing".to_owned())
                })?;
            let delivery_ref = match event.event_type.as_str() {
                "download.requested" => {
                    let inputs_hash = event
                        .payload
                        .get("inputsHash")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            LibraryError::InvalidRecord(
                                "job outbox inputs hash is missing".to_owned(),
                            )
                        })?;
                    jobs.register_job(RegisterJob {
                        command: "library.download.apply".to_owned(),
                        owner_scope: owner_scope.to_owned(),
                        idempotency_key: operation_id.to_owned(),
                        request_hash: inputs_hash.to_owned(),
                        target: event.payload.clone(),
                        confirmation_policy: ConfirmationPolicy::Policy,
                    })?
                    .job
                    .job_id
                }
                "operation.cancelled" => {
                    if let Some(job) =
                        jobs.find_operation(owner_scope, "library.download.apply", operation_id)?
                    {
                        let snapshot = jobs.get_job(owner_scope, &job.job_id)?;
                        let cancel_key = format!("library-operation-cancel:{operation_id}");
                        let cancel_hash = request_hash(&json!({
                            "operationId": operation_id,
                            "jobId": job.job_id,
                        }))?;
                        jobs.request_cancel(
                            owner_scope,
                            &job.job_id,
                            snapshot.last_seq,
                            &cancel_key,
                            &cancel_hash,
                        )?;
                        job.job_id
                    } else {
                        format!("no-job:{operation_id}")
                    }
                }
                other => {
                    return Err(LibraryError::InvalidRecord(format!(
                        "unsupported outbox event {other}"
                    ))
                    .into());
                }
            };
            if library.mark_outbox_delivered(&event.event_id, &delivery_ref)? {
                delivered += 1;
            }
        }
    }
}

fn handle_core_request(state: &DaemonState, request: ValidatedRequest) -> Result<Value, Box<AppError>> {
    let ValidatedRequest {
        envelope,
        command,
        descriptor,
    } = request;
    let request_id = rpc_id_string(&envelope.id);
    let mutation_key = envelope.idempotency_key.as_deref();
    match command {
        ApplicationCommand::SystemStatus(_) => serde_json::to_value(SystemStatusResult {
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            host_api: "1".to_owned(),
            generation_id: state.generation_id.clone(),
            data_dir: state.layout.data_dir.to_string_lossy().into_owned(),
            ready: state.bootstrap_error.lock().is_none(),
        })
        .map_err(|error| Box::new(internal_error(&request_id, error.to_string()))),
        ApplicationCommand::SystemDoctor(_) => {
            let mut checks = doctor_checks(&state.layout);
            if let Some(error) = state.bootstrap_error.lock().as_ref() {
                checks.push(DoctorCheck {
                    name: "signed-baseline".to_owned(),
                    status: DoctorCheckStatus::Error,
                    message: error.clone(),
                });
            }
            let healthy = checks
                .iter()
                .all(|check| check.status != DoctorCheckStatus::Error);
            serde_json::to_value(SystemDoctorResult { healthy, checks })
                .map_err(|error| Box::new(internal_error(&request_id, error.to_string())))
        }
        ApplicationCommand::GamesList(params) => library_value(
            &request_id,
            state
                .library
                .list_games(&state.library_authorization, &params),
        ),
        ApplicationCommand::ProfilesList(params) => library_value(
            &request_id,
            state
                .library
                .list_profiles(&state.library_authorization, &params),
        ),
        ApplicationCommand::ProfilesContentState(params) => {
            profile_content_state_value(&request_id, state, &params.profile_id)
        }
        ApplicationCommand::ProfilesCreate(params) => library_value(
            &request_id,
            state.library.create_profile(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ProfilesUpdate(params) => library_value(
            &request_id,
            state.library.update_profile(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ProfilesActivate(params) => library_value(
            &request_id,
            state.library.activate_profile(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ProfilesDelete(params) => library_value(
            &request_id,
            state.library.delete_profile_command(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ProfilesClone(params) => library_value(
            &request_id,
            state.library.clone_profile(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::GroupsList(params) => library_value(
            &request_id,
            state
                .library
                .list_groups(&state.library_authorization, &params),
        ),
        ApplicationCommand::GroupsCreate(params) => library_value(
            &request_id,
            state.library.create_group(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::GroupsUpdate(params) => library_value(
            &request_id,
            state.library.update_group(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::GroupsMove(params) => library_value(
            &request_id,
            state.library.move_group(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::GroupsDelete(params) => library_value(
            &request_id,
            state.library.delete_group_command(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ModsList(params) => library_value(
            &request_id,
            state
                .library
                .list_mods(&state.library_authorization, &params),
        ),
        ApplicationCommand::ModsGet(params) => library_value(
            &request_id,
            state.library.get_mod(&state.library_authorization, &params),
        ),
        ApplicationCommand::ModsRename(params) => library_value(
            &request_id,
            state.library.rename_mod(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ModsSetEnabled(params) => library_value(
            &request_id,
            state.library.set_mod_enabled(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ModsSetEnabledBatch(params) => library_value(
            &request_id,
            state.library.set_mods_enabled_batch(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ModsMove(params) => library_value(
            &request_id,
            state.library.move_mod(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ModsRemove(params) => library_value(
            &request_id,
            state.library.remove_mod(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ModsCleanupMissing(params) => library_value(
            &request_id,
            state.library.cleanup_missing_mods(
                &state.library_authorization,
                mutation_key.expect("validated mutation has an idempotency key"),
                &params,
            ),
        ),
        ApplicationCommand::ModsSetVersion(params) => {
            let started = state
                .library
                .start_version_download(
                    &state.library_authorization,
                    mutation_key.expect("validated mutation has an idempotency key"),
                    &state.generation_id,
                    &sources::current_lock(state).map_err(|reason| Box::new(AppError {
                        code: "MODULE_UNAVAILABLE".into(), message_key: "source.unprovisioned".into(),
                        details: json!({"reason":reason}), request_id: request_id.clone(),
                        job_id: None, retryable: false, recovery_actions: vec![],
                    }))?,
                    &params,
                )
                .map_err(|error| Box::new(library_error(&request_id, error)))?;
            replay_library_outbox(&state.library, &state.jobs)
                .map_err(|error| Box::new(internal_error(&request_id, error.to_string())))?;
            let operation_id = started.operation.operation_id.clone();
            let job = state
                .jobs
                .find_operation(LOCAL_OWNER_SCOPE, "library.download.apply", &operation_id)
                .and_then(|job| job.ok_or_else(|| JobError::NotFound(operation_id.clone())))
                .map_err(|error| Box::new(job_error(&request_id, error)))?;
            if started.created {
                spawn_download(
                    sources::for_job(state, &job.job_id).map_err(|error| Box::new(internal_error(&request_id, error)))?.downloads,
                    job.job_id.clone(),
                    operation_id,
                    None,
                );
            }
            job_value(&request_id, Ok(job))
        }
        ApplicationCommand::JobsGet(params) => job_value(
            &request_id,
            state.jobs.get_job(LOCAL_OWNER_SCOPE, &params.job_id),
        ),
        ApplicationCommand::JobsWatch(params) => job_value(
            &request_id,
            state.jobs.events_after(
                LOCAL_OWNER_SCOPE,
                &params.job_id,
                params.after_seq,
                params.limit.unwrap_or(256),
            ),
        ),
        ApplicationCommand::JobsCancel(params) => {
            let idempotency_key = envelope
                .idempotency_key
                .as_deref()
                .expect("validated mutation has an idempotency key");
            let hash = request_hash(&envelope.params)
                .map_err(|error| Box::new(job_error(&request_id, error)))?;
            let snapshot = state
                .jobs
                .request_cancel(
                    LOCAL_OWNER_SCOPE,
                    &params.job_id,
                    params.expected_revision,
                    idempotency_key,
                    &hash,
                )
                .map_err(|error| Box::new(job_error(&request_id, error)))?;
            if snapshot.command == "library.download.apply" {
                let target = state
                    .jobs
                    .get_job_target(LOCAL_OWNER_SCOPE, &params.job_id)
                    .map_err(|error| Box::new(job_error(&request_id, error)))?;
                let operation_id = target
                    .get("operationId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        Box::new(internal_error(
                            &request_id,
                            "download job target has no operationId".to_owned(),
                        ))
                    })?;
                sources::for_job(state, &params.job_id).map_err(|error| Box::new(internal_error(&request_id, error)))?.downloads
                    .cancel(&params.job_id, operation_id)
                    .map_err(|error| Box::new(internal_error(&request_id, error.to_string())))?;
            }
            job_value(&request_id, Ok(snapshot))
        }
        ApplicationCommand::ActionsGet(params) => job_value(
            &request_id,
            state.jobs.get_action(LOCAL_OWNER_SCOPE, &params.action_id),
        ),
        ApplicationCommand::ActionsApprove(params) => {
            let idempotency_key = envelope
                .idempotency_key
                .as_deref()
                .expect("validated mutation has an idempotency key");
            let hash = request_hash(&envelope.params)
                .map_err(|error| Box::new(job_error(&request_id, error)))?;
            let job = state
                .jobs
                .approve_action(
                    LOCAL_OWNER_SCOPE,
                    &params.action_id,
                    params.expected_revision,
                    &params.response,
                    idempotency_key,
                    &hash,
                )
                .map_err(|error| Box::new(job_error(&request_id, error)))?;
            let snapshot = state
                .jobs
                .get_job(LOCAL_OWNER_SCOPE, &job.job_id)
                .map_err(|error| Box::new(job_error(&request_id, error)))?;
            if snapshot.command == "library.download.apply"
                && snapshot.state == mintcat_sdk::JobState::AwaitingApproval
            {
                let action = state
                    .jobs
                    .get_action(LOCAL_OWNER_SCOPE, &params.action_id)
                    .map_err(|error| Box::new(job_error(&request_id, error)))?;
                spawn_download(
                    sources::for_job(state, &job.job_id).map_err(|error| Box::new(internal_error(&request_id, error)))?.downloads,
                    job.job_id.clone(),
                    action.operation_id.clone(),
                    Some(action),
                );
            }
            job_value(&request_id, Ok(job))
        }
        ApplicationCommand::ActionsReject(params) => {
            let key = envelope.idempotency_key.as_deref().expect("validated mutation key");
            let hash = request_hash(&envelope.params).map_err(|error| Box::new(job_error(&request_id, error)))?;
            job_value(&request_id, state.jobs.reject_action(
                LOCAL_OWNER_SCOPE, &params.action_id, params.expected_revision, &params.reason, key, &hash,
            ))
        }
        _ => Err(Box::new(AppError {
            code: "MODULE_UNAVAILABLE".to_owned(),
            message_key: "command.module_unavailable".to_owned(),
            details: json!({"method": descriptor.method}),
            request_id,
            job_id: None,
            retryable: false,
            recovery_actions: Vec::<RecoveryAction>::new(),
        })),
    }
}

fn contract_transfer_state(state: RuntimeTransferState) -> ContractTransferState {
    match state {
        RuntimeTransferState::Queued => ContractTransferState::Queued,
        RuntimeTransferState::Downloading => ContractTransferState::Downloading,
        RuntimeTransferState::Verifying => ContractTransferState::Verifying,
        RuntimeTransferState::Publishing => ContractTransferState::Publishing,
        RuntimeTransferState::Succeeded => ContractTransferState::Succeeded,
        RuntimeTransferState::Failed => ContractTransferState::Failed,
        RuntimeTransferState::Cancelled => ContractTransferState::Cancelled,
    }
}

fn profile_content_state_value(
    request_id: &str,
    state: &DaemonState,
    profile_id: &str,
) -> Result<Value, Box<AppError>> {
    let profile_state = state
        .library
        .profile_download_state(&state.library_authorization, profile_id)
        .map_err(|error| Box::new(library_error(request_id, error)))?;
    let capability = profile_state.capability;
    let mut operations = Vec::with_capacity(profile_state.plans.len());
    for plan in profile_state.plans {
        let operation_id = plan.operation.operation_id.clone();
        let job = state
            .jobs
            .find_operation(LOCAL_OWNER_SCOPE, "library.download.apply", &operation_id)
            .map_err(|error| Box::new(job_error(request_id, error)))?;
        let verified_units = plan
            .items
            .iter()
            .filter(|item| item.artifact.is_some())
            .count() as u64;
        let total_units = plan.items.len() as u64;
        let (job_id, job_state, phase, completed_units, reported_total_units) = if let Some(job) =
            job
        {
            let snapshot = state
                .jobs
                .get_job(LOCAL_OWNER_SCOPE, &job.job_id)
                .map_err(|error| Box::new(job_error(request_id, error)))?;
            let latest = if snapshot.last_seq == 0 {
                None
            } else {
                state
                    .jobs
                    .events_after(
                        LOCAL_OWNER_SCOPE,
                        &job.job_id,
                        snapshot.last_seq.saturating_sub(1),
                        1,
                    )
                    .map_err(|error| Box::new(job_error(request_id, error)))?
                    .events
                    .into_iter()
                    .last()
            };
            (
                Some(job.job_id),
                snapshot.state,
                latest
                    .as_ref()
                    .map(|event| event.phase.clone())
                    .unwrap_or_else(|| "download-queued".to_owned()),
                latest
                    .as_ref()
                    .and_then(|event| event.completed_units)
                    .unwrap_or(verified_units),
                latest
                    .as_ref()
                    .and_then(|event| event.total_units)
                    .unwrap_or(total_units),
            )
        } else {
            let (job_state, phase) = match plan.operation.state {
                DownloadOperationState::Queued => (JobState::Queued, "download-queued"),
                DownloadOperationState::Running => (JobState::Running, "download-running"),
                DownloadOperationState::RecoveryRequired => {
                    (JobState::RecoveryRequired, "download-recovery-required")
                }
                DownloadOperationState::Succeeded => (JobState::Succeeded, "download-succeeded"),
                DownloadOperationState::Failed => (JobState::Failed, "download-failed"),
                DownloadOperationState::Cancelled => (JobState::Cancelled, "download-cancelled"),
            };
            (
                None,
                job_state,
                phase.to_owned(),
                verified_units,
                total_units,
            )
        };
        let pending_selection = plan.operation.mode == DownloadMode::PrepareAndApply;
        let mut items = Vec::with_capacity(plan.items.len());
        for item in plan.items {
            let progress = state.downloads
                .transfer_progress(&operation_id, &item.item_id)
                .map_err(|error| Box::new(internal_error(request_id, error.to_string())))?;
            let (
                transfer_id,
                attempt_id,
                transfer_seq,
                transfer_phase,
                transfer_state,
                downloaded_bytes,
                total_bytes,
            ) = match progress {
                Some(progress) => {
                    let transfer_state = Some(contract_transfer_state(progress.transfer.state));
                    match progress.event {
                        Some(event) => (
                            Some(progress.transfer.transfer_id),
                            event.attempt_id,
                            Some(event.seq),
                            Some(event.phase),
                            transfer_state,
                            Some(event.downloaded_bytes),
                            event.total_bytes,
                        ),
                        None => (
                            Some(progress.transfer.transfer_id),
                            progress.transfer.active_attempt_id,
                            Some(progress.transfer.last_seq),
                            None,
                            transfer_state,
                            None,
                            None,
                        ),
                    }
                }
                None => (None, None, None, None, None, None, None),
            };
            items.push(DownloadOverlayItem {
                profile_mod_id: item.profile_mod_id,
                incarnation_id: item.profile_mod_incarnation_id,
                resolved_version: item.resolved_version,
                pending_selection: pending_selection.then_some(item.selection).flatten(),
                verified: item.artifact.is_some(),
                transfer_id,
                attempt_id,
                transfer_seq,
                transfer_phase,
                transfer_state,
                downloaded_bytes,
                total_bytes,
            });
        }
        operations.push(DownloadOverlay {
            operation_id,
            job_id,
            mode: plan.operation.mode,
            state: job_state,
            phase,
            completed_units,
            total_units: reported_total_units,
            items,
        });
    }
    serde_json::to_value(ProfilesContentStateResult {
        profile_id: profile_id.to_owned(),
        content_revision: capability.content_revision,
        writable: capability.writable,
        busy_operation_id: capability.busy_operation_id,
        recovery_required: capability.recovery_required,
        operations,
    })
    .map_err(|error| Box::new(internal_error(request_id, error.to_string())))
}

fn job_value<T: serde::Serialize>(
    request_id: &str,
    result: Result<T, JobError>,
) -> Result<Value, Box<AppError>> {
    let value = result.map_err(|error| Box::new(job_error(request_id, error)))?;
    serde_json::to_value(value)
        .map_err(|error| Box::new(internal_error(request_id, error.to_string())))
}

fn library_value<T: serde::Serialize>(
    request_id: &str,
    result: Result<T, LibraryError>,
) -> Result<Value, Box<AppError>> {
    let value = result.map_err(|error| Box::new(library_error(request_id, error)))?;
    serde_json::to_value(value)
        .map_err(|error| Box::new(internal_error(request_id, error.to_string())))
}

fn library_error(request_id: &str, error: LibraryError) -> AppError {
    let (code, message_key, details, retryable) = match error {
        LibraryError::OwnerUnavailable(reason) => (
            "MODULE_UNAVAILABLE",
            "library.owner_unavailable",
            json!({"reason": reason}),
            false,
        ),
        LibraryError::NotFound(resource) => (
            "NOT_FOUND",
            "library.not_found",
            json!({"resource": resource}),
            false,
        ),
        LibraryError::PermissionDenied(permission) => (
            "PERMISSION_DENIED",
            "library.permission_denied",
            json!({"permission": permission}),
            false,
        ),
        LibraryError::RevisionConflict { expected, actual } => (
            "REVISION_CONFLICT",
            "library.revision_conflict",
            json!({"expected": expected, "actual": actual}),
            true,
        ),
        LibraryError::ResourceBusy { owner_operation_id } => (
            "RESOURCE_BUSY",
            "library.resource_busy",
            json!({"operationId": owner_operation_id, "cancellable": true}),
            true,
        ),
        LibraryError::IdempotencyConflict => (
            "CONFLICT",
            "library.idempotency_conflict",
            json!({"reason": "idempotency-key-reused"}),
            false,
        ),
        LibraryError::StaleOperation => (
            "STALE_TARGET",
            "library.operation_stale",
            json!({"reason": "operation-stale"}),
            false,
        ),
        LibraryError::StaleFence { expected, actual } => (
            "STALE_FENCE",
            "library.fence_stale",
            json!({"expected": expected, "actual": actual}),
            false,
        ),
        LibraryError::StaleTarget(target) => (
            "STALE_TARGET",
            "library.target_stale",
            json!({"target": target}),
            false,
        ),
        LibraryError::OperationState(state) => (
            "CONFLICT",
            "library.operation_state",
            json!({"state": state}),
            false,
        ),
        LibraryError::ArtifactNotVerified(artifact) => (
            "ARTIFACT_NOT_VERIFIED",
            "library.artifact_not_verified",
            json!({"artifact": artifact}),
            false,
        ),
        LibraryError::LibraryNotEmpty => (
            "CONFLICT",
            "library.not_empty",
            json!({"reason": "library-not-empty"}),
            false,
        ),
        LibraryError::BackupExists(path) => (
            "CONFLICT",
            "library.backup_exists",
            json!({"path": path}),
            false,
        ),
        LibraryError::UnsupportedBackup(format) => (
            "INVALID_ARGUMENT",
            "library.backup_unsupported",
            json!({"format": format}),
            false,
        ),
        LibraryError::InvalidRecord(message)
        | LibraryError::LegacyIntegrity(message)
        | LibraryError::MissingLegacyTable(message) => (
            "INVALID_ARGUMENT",
            "library.invalid_argument",
            json!({"message": message}),
            false,
        ),
        LibraryError::MissingLegacyReference { entity, legacy_id } => (
            "INVALID_ARGUMENT",
            "library.invalid_legacy_reference",
            json!({"entity": entity, "legacyId": legacy_id}),
            false,
        ),
        LibraryError::CredentialVault { provider, message } => (
            "INTERNAL",
            "library.credential_store_failed",
            json!({"provider": provider, "message": message}),
            false,
        ),
        LibraryError::CredentialCleanup {
            credential_ref,
            message,
        } => (
            "INTERNAL",
            "library.credential_cleanup_failed",
            json!({"credentialRef": credential_ref, "message": message}),
            false,
        ),
        LibraryError::UninitializedLayout
        | LibraryError::Sqlite(_)
        | LibraryError::Poisoned
        | LibraryError::UnsupportedSchema(_)
        | LibraryError::Json(_)
        | LibraryError::Io(_) => (
            "INTERNAL",
            "library.storage_failed",
            json!({"category": "library-database"}),
            false,
        ),
    };
    AppError {
        code: code.to_owned(),
        message_key: message_key.to_owned(),
        details,
        request_id: request_id.to_owned(),
        job_id: None,
        retryable,
        recovery_actions: Vec::new(),
    }
}

fn job_error(request_id: &str, error: JobError) -> AppError {
    let (code, message_key, details) = match error {
        JobError::NotFound(resource) => (
            "NOT_FOUND",
            "jobs.not_found",
            json!({ "resource": resource }),
        ),
        JobError::IdempotencyConflict => (
            "CONFLICT",
            "jobs.idempotency_conflict",
            json!({ "reason": "idempotency-key-reused" }),
        ),
        JobError::RevisionConflict { expected, actual } => (
            "REVISION_CONFLICT",
            "jobs.revision_conflict",
            json!({ "expected": expected, "actual": actual }),
        ),
        JobError::InvalidTransition { from, to } => (
            "CONFLICT",
            "jobs.invalid_transition",
            json!({ "from": from, "to": to }),
        ),
        JobError::ActionState(state) => (
            "CONFLICT",
            "actions.invalid_state",
            json!({ "state": state }),
        ),
        JobError::ActionExpired => (
            "STALE_PLAN",
            "actions.expired",
            json!({ "reason": "expired" }),
        ),
        JobError::InvalidResponse(message) => (
            "INVALID_ARGUMENT",
            "actions.response_invalid",
            json!({ "message": message }),
        ),
        JobError::StalePlan => (
            "STALE_PLAN",
            "actions.binding_stale",
            json!({ "reason": "binding-mismatch" }),
        ),
        JobError::PermissionDenied => (
            "PERMISSION_DENIED",
            "jobs.permission_denied",
            json!({ "scope": LOCAL_OWNER_SCOPE }),
        ),
        JobError::InvalidRetention
        | JobError::Kernel(_)
        | JobError::Sqlite(_)
        | JobError::Json(_) => (
            "INTERNAL",
            "jobs.storage_failed",
            json!({ "category": "kernel-database" }),
        ),
    };
    AppError {
        code: code.to_owned(),
        message_key: message_key.to_owned(),
        details,
        request_id: request_id.to_owned(),
        job_id: None,
        retryable: false,
        recovery_actions: Vec::new(),
    }
}

fn doctor_checks(layout: &RuntimeLayout) -> Vec<DoctorCheck> {
    let mut checks = vec![DoctorCheck {
        name: "runtime-layout".to_owned(),
        status: DoctorCheckStatus::Ok,
        message: format!(
            "vNext layout {} is initialized at {}",
            layout.layout_version,
            layout.data_dir.display()
        ),
    }];

    match write_probe(&layout.runtime_dir) {
        Ok(()) => checks.push(DoctorCheck {
            name: "data-directory-write".to_owned(),
            status: DoctorCheckStatus::Ok,
            message: "runtime directory accepted an atomic write probe".to_owned(),
        }),
        Err(error) => checks.push(DoctorCheck {
            name: "data-directory-write".to_owned(),
            status: DoctorCheckStatus::Error,
            message: error,
        }),
    }

    checks.push(endpoint_permission_check(&layout.endpoint));
    checks.push(DoctorCheck {
        name: "peer-authentication".to_owned(),
        status: DoctorCheckStatus::Ok,
        message: "local transport authenticates the operating-system peer identity".to_owned(),
    });
    checks
}

fn write_probe(directory: &Path) -> Result<(), String> {
    let path = directory.join(format!(".doctor-{}", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| format!("could not create write probe: {error}"))?;
    file.write_all(b"mintcat-doctor")
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("could not persist write probe: {error}"))?;
    fs::remove_file(&path).map_err(|error| format!("could not remove write probe: {error}"))
}

#[cfg(unix)]
fn endpoint_permission_check(endpoint: &Path) -> DoctorCheck {
    use std::os::unix::fs::PermissionsExt;

    match fs::metadata(endpoint) {
        Ok(metadata) if metadata.permissions().mode() & 0o077 == 0 => DoctorCheck {
            name: "endpoint-permissions".to_owned(),
            status: DoctorCheckStatus::Ok,
            message: "local socket is restricted to the owning user".to_owned(),
        },
        Ok(metadata) => DoctorCheck {
            name: "endpoint-permissions".to_owned(),
            status: DoctorCheckStatus::Error,
            message: format!(
                "local socket permissions are {:o}, expected no group/other access",
                metadata.permissions().mode() & 0o777
            ),
        },
        Err(error) => DoctorCheck {
            name: "endpoint-permissions".to_owned(),
            status: DoctorCheckStatus::Error,
            message: format!("could not inspect local socket: {error}"),
        },
    }
}

#[cfg(not(unix))]
fn endpoint_permission_check(_endpoint: &Path) -> DoctorCheck {
    DoctorCheck {
        name: "endpoint-permissions".to_owned(),
        status: DoctorCheckStatus::Ok,
        message: "named pipe rejects remote clients and uses the owner security descriptor"
            .to_owned(),
    }
}

fn internal_error(request_id: &str, message: String) -> AppError {
    AppError {
        code: "INTERNAL".to_owned(),
        message_key: "system.internal".to_owned(),
        details: json!({"message": message}),
        request_id: request_id.to_owned(),
        job_id: None,
        retryable: false,
        recovery_actions: Vec::<RecoveryAction>::new(),
    }
}

fn rpc_id_string(id: &JsonRpcId) -> String {
    match id {
        JsonRpcId::String(value) => value.clone(),
        JsonRpcId::Integer(value) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use mintcat_library_service::{
        CredentialVault, LegacyCredential, LibraryAuthorization, StartDownloadOperation,
        StartOperationItem, CONTENT_WRITE_PERMISSION,
    };
    use mintcat_sdk::{DownloadMode, JobState};
    use tempfile::tempdir;

    use super::*;

    struct EmptyVault;

    impl CredentialVault for EmptyVault {
        fn store(&self, _credential: LegacyCredential<'_>) -> Result<String, String> {
            Err("fixture unexpectedly contained a credential".to_owned())
        }

        fn delete(&self, _credential_ref: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn legacy_fixture() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/baseline/legacy/mintcat-v0.6.sqlite")
            .leak()
    }

    #[tokio::test]
    async fn readonly_rescue_is_not_reported_as_a_ready_healthy_baseline() {
        let sandbox = tempdir().unwrap();
        let paths = RuntimePaths::new(sandbox.path().join("data")).unwrap();
        let daemon = BoundDaemon::bind(&paths, RuntimeEnvironment::Test).unwrap();
        *daemon.state.bootstrap_error.lock() = Some("signed owner failed to start".into());
        composition::resume_after_backup_recovery(&daemon.state).await.unwrap();
        for (method, field) in [("system.status", "ready"), ("system.doctor", "healthy")] {
            let request = mintcat_sdk::decode_request_value(json!({
                "jsonrpc": "2.0", "id": method, "method": method, "contractVersion": 1,
                "sessionGeneration": daemon.state.generation_id, "deadlineMs": 5_000,
                "params": {}
            })).unwrap();
            let result = handle_core_request(&daemon.state, request).unwrap();
            assert_eq!(result[field], false);
            if method == "system.doctor" {
                assert!(result["checks"].as_array().unwrap().iter().any(|check|
                    check["name"] == "signed-baseline" && check["status"] == "error"));
            }
        }
    }

    #[tokio::test]
    async fn library_outbox_replay_registers_one_kernel_job_and_propagates_cancel() {
        let sandbox = tempdir().unwrap();
        let paths = RuntimePaths::new(sandbox.path().join("data")).unwrap();
        let daemon = BoundDaemon::bind(&paths, RuntimeEnvironment::Test).unwrap();
        let legacy = sandbox.path().join("legacy.sqlite");
        fs::copy(legacy_fixture(), &legacy).unwrap();
        daemon
            .library_store()
            .import_legacy(&legacy, &EmptyVault)
            .unwrap();
        let snapshot = daemon.library_store().snapshot().unwrap();
        let profile = snapshot
            .profiles
            .iter()
            .find(|record| record.profile_id == "profile:legacy:1")
            .unwrap();
        let profile_mod = snapshot
            .profile_mods
            .iter()
            .find(|record| record.profile_id == profile.profile_id && record.enabled)
            .unwrap();
        let library_mod = snapshot
            .mods
            .iter()
            .find(|record| record.mod_id == profile_mod.mod_id)
            .unwrap();
        let version = snapshot
            .versions
            .iter()
            .find(|record| {
                record.mod_id == library_mod.mod_id
                    && Some(record.version_id.as_str()) == profile_mod.used_version.as_deref()
            })
            .unwrap();
        let authorization = LibraryAuthorization::new(
            profile.owner_scope.clone(),
            [CONTENT_WRITE_PERMISSION.to_owned()],
        );
        let started = daemon
            .library_store()
            .start_download_operation(
                &authorization,
                &StartDownloadOperation {
                    idempotency_scope: "library.download".to_owned(),
                    idempotency_key: "daemon-outbox".to_owned(),
                    profile_id: profile.profile_id.clone(),
                    expected_content_revision: profile.content_revision,
                    target_incarnation_id: profile.incarnation_id.clone(),
                    owner_generation: daemon.ready().generation_id,
                    module_lock_digest: format!("sha256:{}", "a".repeat(64)),
                    mode: DownloadMode::PrepareAndApply,
                    items: vec![StartOperationItem {
                        item_id: "primary".to_owned(),
                        profile_mod_id: profile_mod.profile_mod_id.clone(),
                        profile_mod_incarnation_id: profile_mod.incarnation_id.clone(),
                        mod_id: library_mod.mod_id.clone(),
                        source_id: library_mod.source_id.clone(),
                        source_mod_id: library_mod.remote_mod_id.clone(),
                        remote_game_key: library_mod.remote_game_key.clone(),
                        source_file_id: version.remote_file_id.clone(),
                        resolved_version: version.version_id.clone(),
                        expected_digest: None,
                        validator: None,
                        selection: None,
                    }],
                },
            )
            .unwrap();

        assert_eq!(daemon.replay_library_outbox().unwrap(), 1);
        assert_eq!(daemon.replay_library_outbox().unwrap(), 0);
        let job = daemon
            .job_store()
            .find_operation(
                profile.owner_scope.as_str(),
                "library.download.apply",
                &started.operation.operation_id,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            daemon
                .job_store()
                .get_job(&profile.owner_scope, &job.job_id)
                .unwrap()
                .state,
            JobState::Queued
        );

        daemon
            .library_store()
            .cancel_download_operation(&authorization, &started.operation.operation_id)
            .unwrap();
        assert_eq!(daemon.replay_library_outbox().unwrap(), 1);
        assert_eq!(daemon.replay_library_outbox().unwrap(), 0);
        assert_eq!(
            daemon
                .job_store()
                .get_job(&profile.owner_scope, &job.job_id)
                .unwrap()
                .state,
            JobState::Cancelling
        );
    }
    #[tokio::test]
    async fn portable_restore_rejects_forged_outbox_without_creating_live_kernel_jobs() {
        use mintcat_feature_backup::{BackupArchive, BackupRuntimeState, BackupService, ModuleVersion, PortableSnapshot};
        use mintcat_library_service::OutboxRecord;
        use sha2::{Digest, Sha256};
        let sandbox=tempdir().unwrap();
        let source_layout=RuntimePaths::new(sandbox.path().join("source")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
        let source=LibraryStore::open(&source_layout).unwrap();
        source.import_legacy(&legacy_fixture(),&EmptyVault).unwrap();
        let runtime=||Ok(BackupRuntimeState {
            blockers:Default::default(),
            modules:std::collections::BTreeMap::from([("mintcat.library".into(),ModuleVersion {version:"0.1.0".into(),schema_version:5})]),
        });
        let bytes=BackupService::new(source_layout,source).unwrap().export_bytes(runtime).unwrap();
        let mut archive:BackupArchive=serde_json::from_slice(&bytes).unwrap();
        let mut portable:PortableSnapshot=serde_json::from_str(&archive.content).unwrap();
        let operation_id="forged-portable-task";
        portable.library.outbox.push(OutboxRecord {
            event_seq:1,event_id:"forged-portable-event".into(),operation_id:None,
            event_type:"download.requested".into(),destination:"kernel-job".into(),
            payload_json:json!({"operationId":operation_id,"ownerScope":LOCAL_OWNER_SCOPE,"inputsHash":format!("sha256:{}","1".repeat(64))}).to_string(),
            delivery_state:"pending".into(),delivery_ref:None,created_at_ms:0,delivered_at_ms:None,
        });
        archive.content=serde_json::to_string(&portable).unwrap();
        archive.manifest.content_digest=format!("sha256:{:x}",Sha256::digest(archive.content.as_bytes()));
        archive.manifest.content_size=archive.content.len() as u64;
        let target_paths=RuntimePaths::new(sandbox.path().join("target")).unwrap();
        let daemon=BoundDaemon::bind(&target_paths,RuntimeEnvironment::Test).unwrap();
        let layout=target_paths.initialize(RuntimeEnvironment::Test).unwrap();
        let target=BackupService::new(layout,daemon.library_store().clone()).unwrap();
        assert!(target.restore_bytes(&serde_json::to_vec(&archive).unwrap(),runtime).is_err());
        assert_eq!(daemon.library_store().snapshot().unwrap(),mintcat_library_service::LibrarySnapshot::default());
        assert_eq!(daemon.replay_library_outbox().unwrap(),0);
        assert!(daemon.job_store().find_operation(LOCAL_OWNER_SCOPE,"library.download.apply",operation_id).unwrap().is_none());
        target.restore_bytes(&bytes,runtime).unwrap();
        assert_eq!(daemon.library_store().snapshot().unwrap().profiles,portable.library.profiles);
        assert_eq!(daemon.replay_library_outbox().unwrap(),0);
        assert!(daemon.job_store().find_operation(LOCAL_OWNER_SCOPE,"library.download.apply",operation_id).unwrap().is_none());
    }
}
