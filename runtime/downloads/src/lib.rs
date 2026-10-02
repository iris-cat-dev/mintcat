use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use mintcat_library_service::{
    AppliedArtifact, ApplyOperationResult, DownloadExecutionItem, DownloadOperationState,
    LibraryAuthorization, LibraryError, LibraryStore,
};
use mintcat_runtime_artifacts::{ArtifactError, ArtifactGrant, ArtifactStore};
use mintcat_runtime_broker::{CancellationToken, InteractionContext};
use mintcat_runtime_jobs::{EventUpdate, JobError, JobStore, ResumePolicy, WorkerCheckpoint};
use mintcat_runtime_transfers::{
    TransferError, TransferEvent, TransferFileKey, TransferManager, TransferRequest,
    TransferSharing, TransferSnapshot, TransferValidation,
};
use mintcat_sdk::{ActionRequired, AppError, JobState, ModRef};
use mintcat_source_api::{
    DownloadDescriptor, DownloadResolution, SourceError, SourceGameBinding, SourceProvider,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

const DEFAULT_INTERACTION_TTL_MS: i64 = 15 * 60 * 1_000;

#[derive(Debug, Error)]
pub enum DownloadCoordinatorError {
    #[error(transparent)]
    Library(#[from] LibraryError),
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Transfer(#[from] TransferError),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Job(#[from] JobError),
    #[error("download catalog lock is poisoned")]
    CatalogPoisoned,
    #[error("download source is not registered: {0}")]
    SourceNotRegistered(String),
    #[error("download game binding is not registered: {source_id}/{remote_game_key}")]
    GameBindingNotRegistered {
        source_id: String,
        remote_game_key: String,
    },
    #[error("download plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("download operation previously failed: {0}")]
    OperationFailed(String),
    #[error("download failure could not be persisted: {0}")]
    FailurePersistence(String),
}

impl DownloadCoordinatorError {
    fn code(&self) -> &'static str {
        match self {
            Self::Library(_) => "LIBRARY_TRANSACTION_FAILED",
            Self::Source(SourceError::Unauthorized) => "SOURCE_UNAUTHORIZED",
            Self::Source(SourceError::RateLimited) => "SOURCE_RATE_LIMITED",
            Self::Source(SourceError::NotFound(_)) => "SOURCE_FILE_NOT_FOUND",
            Self::Source(_) => "SOURCE_DOWNLOAD_FAILED",
            Self::Transfer(TransferError::Cancelled) => "DOWNLOAD_CANCELLED",
            Self::Transfer(_) => "TRANSFER_FAILED",
            Self::Artifact(_) => "ARTIFACT_UNAVAILABLE",
            Self::Job(_) => "JOB_STATE_FAILED",
            Self::CatalogPoisoned => "DOWNLOAD_CATALOG_UNAVAILABLE",
            Self::SourceNotRegistered(_) => "SOURCE_NOT_REGISTERED",
            Self::GameBindingNotRegistered { .. } => "GAME_BINDING_NOT_REGISTERED",
            Self::InvalidPlan(_) => "INVALID_DOWNLOAD_PLAN",
            Self::OperationFailed(_) => "DOWNLOAD_OPERATION_FAILED",
            Self::FailurePersistence(_) => "DOWNLOAD_FAILURE_PERSISTENCE_FAILED",
        }
    }

    fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Source(SourceError::RateLimited)
                | Self::Source(SourceError::Unavailable(_))
                | Self::Source(SourceError::Network(_))
                | Self::Transfer(TransferError::Broker(_))
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum DownloadExecutionResult {
    Succeeded { result: ApplyOperationResult },
    AwaitingAction { action: ActionRequired },
    Cancelled { operation_id: String },
}

#[derive(Clone, Default)]
pub struct DownloadCatalog {
    inner: Arc<RwLock<CatalogState>>,
}

#[derive(Default)]
struct CatalogState {
    providers: HashMap<String, ProviderEntry>,
    games: HashMap<(String, String), SourceGameBinding>,
}

#[derive(Clone)]
struct ProviderEntry {
    provider: Arc<dyn SourceProvider>,
    module_lock_digest: String,
}

impl DownloadCatalog {
    pub fn provider_module_lock(&self, source_id: &str) -> Result<Option<String>, DownloadCoordinatorError> {
        let catalog = self.inner.read().map_err(|_| DownloadCoordinatorError::CatalogPoisoned)?;
        Ok(catalog.providers.get(source_id).map(|provider| provider.module_lock_digest.clone()))
    }

    pub fn register_provider(
        &self,
        provider: Arc<dyn SourceProvider>,
        module_lock_digest: impl Into<String>,
    ) -> Result<(), DownloadCoordinatorError> {
        let source_id = provider.source_id();
        let module_lock_digest = module_lock_digest.into();
        if source_id.trim().is_empty() || !is_sha256(&module_lock_digest) {
            return Err(DownloadCoordinatorError::InvalidPlan(
                "provider identity or module lock digest is invalid".to_owned(),
            ));
        }
        let mut catalog = self
            .inner
            .write()
            .map_err(|_| DownloadCoordinatorError::CatalogPoisoned)?;
        if catalog.providers.contains_key(source_id) {
            return Err(DownloadCoordinatorError::InvalidPlan(format!(
                "source provider {source_id} is already registered"
            )));
        }
        catalog.providers.insert(
            source_id.to_owned(),
            ProviderEntry {
                provider,
                module_lock_digest,
            },
        );
        Ok(())
    }

    pub fn register_game(
        &self,
        source_id: &str,
        binding: SourceGameBinding,
    ) -> Result<(), DownloadCoordinatorError> {
        if source_id.trim().is_empty()
            || binding.game_id.trim().is_empty()
            || binding.remote_game_key.trim().is_empty()
            || binding.provider_game_id.trim().is_empty()
        {
            return Err(DownloadCoordinatorError::InvalidPlan(
                "source game binding fields cannot be empty".to_owned(),
            ));
        }
        let mut catalog = self
            .inner
            .write()
            .map_err(|_| DownloadCoordinatorError::CatalogPoisoned)?;
        if !catalog.providers.contains_key(source_id) {
            return Err(DownloadCoordinatorError::SourceNotRegistered(
                source_id.to_owned(),
            ));
        }
        let key = (source_id.to_owned(), binding.remote_game_key.clone());
        if catalog.games.contains_key(&key) {
            return Err(DownloadCoordinatorError::InvalidPlan(format!(
                "source game binding {source_id} is already registered"
            )));
        }
        catalog.games.insert(key, binding);
        Ok(())
    }

    /// Resolves only an exact registered signed game descriptor. An omitted
    /// remote key is accepted only when this source has one binding for the game.
    pub fn lookup(
        &self,
        source_id: &str,
        game_id: &str,
        remote_game_key: Option<&str>,
    ) -> Result<(Arc<dyn SourceProvider>, SourceGameBinding, String), DownloadCoordinatorError> {
        let catalog = self.inner.read().map_err(|_| DownloadCoordinatorError::CatalogPoisoned)?;
        let provider = catalog.providers.get(source_id).ok_or_else(|| DownloadCoordinatorError::SourceNotRegistered(source_id.to_owned()))?;
        let mut matches = catalog.games.iter().filter(|((source, remote), binding)| {
            source == source_id && binding.game_id == game_id
                && remote_game_key.is_none_or(|key| key == remote)
        });
        let binding = matches.next().map(|(_, binding)| binding.clone()).ok_or_else(|| DownloadCoordinatorError::GameBindingNotRegistered {
            source_id: source_id.to_owned(), remote_game_key: remote_game_key.unwrap_or("").to_owned(),
        })?;
        if matches.next().is_some() {
            return Err(DownloadCoordinatorError::InvalidPlan("source game binding is ambiguous; remoteGameKey is required".into()));
        }
        Ok((provider.provider.clone(), binding, provider.module_lock_digest.clone()))
    }

    fn resolve(
        &self,
        source_id: &str,
        remote_game_key: &str,
    ) -> Result<(ProviderEntry, SourceGameBinding), DownloadCoordinatorError> {
        let catalog = self
            .inner
            .read()
            .map_err(|_| DownloadCoordinatorError::CatalogPoisoned)?;
        let provider =
            catalog.providers.get(source_id).cloned().ok_or_else(|| {
                DownloadCoordinatorError::SourceNotRegistered(source_id.to_owned())
            })?;
        let binding = catalog
            .games
            .get(&(source_id.to_owned(), remote_game_key.to_owned()))
            .cloned()
            .ok_or_else(|| DownloadCoordinatorError::GameBindingNotRegistered {
                source_id: source_id.to_owned(),
                remote_game_key: remote_game_key.to_owned(),
            })?;
        Ok((provider, binding))
    }
}

#[derive(Clone)]
pub struct DownloadCoordinator {
    library: LibraryStore,
    authorization: LibraryAuthorization,
    jobs: JobStore,
    transfers: TransferManager,
    artifacts: ArtifactStore,
    catalog: DownloadCatalog,
    interaction_ttl_ms: i64,
    active_consumers: Arc<Mutex<HashMap<String, Vec<ConsumerAttachment>>>>,
    transfer_runs: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
}

#[derive(Debug, Clone)]
struct ConsumerAttachment {
    transfer_id: String,
    consumer_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActiveTransferProgress {
    pub transfer: TransferSnapshot,
    pub event: Option<TransferEvent>,
}

impl DownloadCoordinator {
    pub fn new(
        library: LibraryStore,
        authorization: LibraryAuthorization,
        jobs: JobStore,
        transfers: TransferManager,
        artifacts: ArtifactStore,
        catalog: DownloadCatalog,
    ) -> Self {
        Self {
            library,
            authorization,
            jobs,
            transfers,
            artifacts,
            catalog,
            interaction_ttl_ms: DEFAULT_INTERACTION_TTL_MS,
            active_consumers: Arc::new(Mutex::new(HashMap::new())),
            transfer_runs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn catalog(&self) -> &DownloadCatalog {
        &self.catalog
    }

    pub fn transfer_progress(
        &self,
        operation_id: &str,
        item_id: &str,
    ) -> Result<Option<ActiveTransferProgress>, DownloadCoordinatorError> {
        let consumer_id = format!("library-operation:{operation_id}:{item_id}");
        let attachment = self
            .active_consumers
            .lock()
            .map_err(|_| DownloadCoordinatorError::CatalogPoisoned)?
            .get(operation_id)
            .and_then(|attachments| {
                attachments
                    .iter()
                    .find(|attachment| attachment.consumer_id == consumer_id)
                    .cloned()
            });
        let Some(attachment) = attachment else {
            return Ok(None);
        };
        Ok(Some(ActiveTransferProgress {
            transfer: self.transfers.get(&attachment.transfer_id)?,
            event: self.transfers.latest_event(&attachment.transfer_id)?,
        }))
    }

    pub async fn execute(
        &self,
        job_id: &str,
        operation_id: &str,
        continuation: Option<&ActionRequired>,
    ) -> Result<DownloadExecutionResult, DownloadCoordinatorError> {
        let outcome = self.execute_inner(job_id, operation_id, continuation).await;
        self.release_consumers(operation_id);
        match outcome {
            Ok(result) => Ok(result),
            Err(_) if self.is_cancelled(job_id, operation_id) => {
                self.finish_cancel(job_id, operation_id)?;
                Ok(DownloadExecutionResult::Cancelled {
                    operation_id: operation_id.to_owned(),
                })
            }
            Err(error) => {
                if let Err(persistence) = self.persist_failure(job_id, operation_id, &error) {
                    return Err(DownloadCoordinatorError::FailurePersistence(format!(
                        "{error}; {persistence}"
                    )));
                }
                Err(error)
            }
        }
    }

    pub fn cancel(
        &self,
        job_id: &str,
        operation_id: &str,
    ) -> Result<DownloadExecutionResult, DownloadCoordinatorError> {
        self.library
            .cancel_download_operation(&self.authorization, operation_id)?;
        self.release_consumers(operation_id);
        self.jobs.cancel_at_safe_point(job_id)?;
        Ok(DownloadExecutionResult::Cancelled {
            operation_id: operation_id.to_owned(),
        })
    }

    async fn execute_inner(
        &self,
        job_id: &str,
        operation_id: &str,
        continuation: Option<&ActionRequired>,
    ) -> Result<DownloadExecutionResult, DownloadCoordinatorError> {
        let mut plan = self
            .library
            .download_execution_plan(&self.authorization, operation_id)?;
        match plan.operation.state {
            DownloadOperationState::Succeeded => {
                let result_value = plan.operation.result.ok_or_else(|| {
                    DownloadCoordinatorError::InvalidPlan(
                        "succeeded operation has no result".to_owned(),
                    )
                })?;
                let result = serde_json::from_value(result_value.clone())
                    .map_err(|error| DownloadCoordinatorError::InvalidPlan(error.to_string()))?;
                let job = self
                    .jobs
                    .get_job(self.authorization.owner_scope(), job_id)?;
                match job.state {
                    JobState::RecoveryRequired => {
                        self.jobs.finish_recovered_commit(job_id, result_value)?;
                    }
                    JobState::Committing => {
                        self.jobs.finish_commit(job_id, result_value)?;
                    }
                    JobState::Succeeded => {}
                    _ => {
                        return Err(DownloadCoordinatorError::InvalidPlan(format!(
                            "succeeded operation has job state {:?}",
                            job.state
                        )));
                    }
                }
                return Ok(DownloadExecutionResult::Succeeded { result });
            }
            DownloadOperationState::Cancelled => {
                self.jobs.cancel_at_safe_point(job_id)?;
                return Ok(DownloadExecutionResult::Cancelled {
                    operation_id: operation_id.to_owned(),
                });
            }
            DownloadOperationState::Failed => {
                return Err(DownloadCoordinatorError::OperationFailed(
                    plan.operation
                        .result
                        .map(|result| result.to_string())
                        .unwrap_or_else(|| operation_id.to_owned()),
                ));
            }
            DownloadOperationState::RecoveryRequired => {
                self.library.resume_download_operation(
                    &self.authorization,
                    operation_id,
                    plan.operation.fence.expected_content_revision,
                    self.jobs.generation_id(),
                )?;
                plan = self
                    .library
                    .download_execution_plan(&self.authorization, operation_id)?;
            }
            DownloadOperationState::Queued | DownloadOperationState::Running => {}
        }
        if continuation.is_some_and(|action| action.operation_id.as_str() != operation_id) {
            return Err(DownloadCoordinatorError::InvalidPlan(
                "interaction action belongs to another operation".to_owned(),
            ));
        }
        if continuation.is_none() {
            let prepared = self.jobs.append_event(
                job_id,
                EventUpdate::state("download-preparing", JobState::Preparing),
            )?;
            self.save_checkpoint(job_id, operation_id, prepared.last_seq)?;
        }
        self.library.mark_download_running(
            operation_id,
            plan.operation.fence.fence_epoch,
            &plan.operation.fence.owner_generation,
        )?;

        let mut continuation = continuation;
        let mut job_running = false;
        let mut grants: Vec<(String, ArtifactGrant)> = Vec::with_capacity(plan.items.len());
        let total_units = plan.items.len() as u64;
        for item in &plan.items {
            if self.checkpoint_cancel(job_id, operation_id)? {
                return Ok(DownloadExecutionResult::Cancelled {
                    operation_id: operation_id.to_owned(),
                });
            }
            if let Some(artifact) = &item.artifact {
                if continuation.is_none() && !job_running {
                    self.append_progress(
                        job_id,
                        operation_id,
                        grants.len() as u64,
                        total_units,
                        None,
                    )?;
                    job_running = true;
                }
                grants.push((item.item_id.clone(), self.artifacts.grant_blob(artifact)?));
                self.append_progress(
                    job_id,
                    operation_id,
                    grants.len() as u64,
                    total_units,
                    Some(item),
                )?;
                continue;
            }
            let (provider, game) = self
                .catalog
                .resolve(&item.source_id, &item.remote_game_key)?;
            let mod_ref = item_mod_ref(item);
            let resumed_action = continuation.is_some();
            let descriptor = if let Some(action) = continuation.take() {
                match provider
                    .provider
                    .resume_download_action(action, &game, &mod_ref, &CancellationToken::default())
                    .await
                {
                    Ok(descriptor) => descriptor,
                    Err(SourceError::InteractionRequired { .. }) => {
                        match self
                            .begin_download(job_id, &plan, item, &provider, &game, &mod_ref)
                            .await?
                        {
                            DownloadResolution::Ready(descriptor) => descriptor,
                            DownloadResolution::InteractionRequired(action) => {
                                return Ok(DownloadExecutionResult::AwaitingAction { action });
                            }
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            } else {
                match self
                    .begin_download(job_id, &plan, item, &provider, &game, &mod_ref)
                    .await?
                {
                    DownloadResolution::Ready(descriptor) => descriptor,
                    DownloadResolution::InteractionRequired(action) => {
                        return Ok(DownloadExecutionResult::AwaitingAction { action });
                    }
                }
            };
            if resumed_action || !job_running {
                self.append_progress(job_id, operation_id, grants.len() as u64, total_units, None)?;
                job_running = true;
            }
            let request = transfer_request(item, descriptor)?;
            let consumer_id = format!("library-operation:{operation_id}:{}", item.item_id);
            let transfer = self.transfers.attach(&consumer_id, &request)?;
            self.track_consumer(
                operation_id,
                ConsumerAttachment {
                    transfer_id: transfer.transfer_id.clone(),
                    consumer_id: consumer_id.clone(),
                },
            )?;
            self.run_transfer_shared(&transfer.transfer_id, &request)
                .await?;
            if self.checkpoint_cancel(job_id, operation_id)? {
                return Ok(DownloadExecutionResult::Cancelled {
                    operation_id: operation_id.to_owned(),
                });
            }
            let grant = self
                .transfers
                .artifact_for_consumer(&transfer.transfer_id, &consumer_id)?;
            self.library.record_download_artifact(
                &self.authorization,
                operation_id,
                plan.operation.fence.fence_epoch,
                &plan.operation.fence.owner_generation,
                &item.item_id,
                &grant,
            )?;
            grants.push((item.item_id.clone(), grant));
            self.append_progress(
                job_id,
                operation_id,
                grants.len() as u64,
                total_units,
                Some(item),
            )?;
            self.library.heartbeat_download_operation(
                operation_id,
                plan.operation.fence.fence_epoch,
                &plan.operation.fence.owner_generation,
            )?;
        }
        if continuation.is_some() {
            return Err(DownloadCoordinatorError::InvalidPlan(
                "interaction action did not match an unresolved operation item".to_owned(),
            ));
        }
        if self.checkpoint_cancel(job_id, operation_id)? {
            return Ok(DownloadExecutionResult::Cancelled {
                operation_id: operation_id.to_owned(),
            });
        }
        self.jobs.begin_commit(job_id)?;
        let applied = grants
            .iter()
            .map(|(item_id, grant)| AppliedArtifact { item_id, grant })
            .collect::<Vec<_>>();
        let result = match self.library.apply_download_operation(
            &self.authorization,
            operation_id,
            plan.operation.fence.fence_epoch,
            &plan.operation.fence.owner_generation,
            &applied,
        ) {
            Ok(result) => result,
            Err(error) => {
                self.jobs.mark_commit_unknown(job_id)?;
                return Err(error.into());
            }
        };
        self.jobs.finish_commit(
            job_id,
            serde_json::to_value(&result).map_err(LibraryError::from)?,
        )?;
        plan.operation.state = DownloadOperationState::Succeeded;
        Ok(DownloadExecutionResult::Succeeded { result })
    }

    fn append_progress(
        &self,
        job_id: &str,
        operation_id: &str,
        completed_units: u64,
        total_units: u64,
        item: Option<&DownloadExecutionItem>,
    ) -> Result<(), DownloadCoordinatorError> {
        let mut update = EventUpdate::state("download-running", JobState::Running);
        update.completed_units = Some(completed_units);
        update.total_units = Some(total_units);
        update.message_key = Some("download.progress".to_owned());
        update.details = item.map(|item| {
            json!({
                "profileModId": item.profile_mod_id,
                "incarnationId": item.profile_mod_incarnation_id,
                "resolvedVersion": item.resolved_version,
            })
        });
        let snapshot = self.jobs.append_event(job_id, update)?;
        self.save_checkpoint(job_id, operation_id, snapshot.last_seq)
    }

    async fn begin_download(
        &self,
        job_id: &str,
        plan: &mintcat_library_service::DownloadExecutionPlan,
        item: &DownloadExecutionItem,
        provider: &ProviderEntry,
        game: &SourceGameBinding,
        mod_ref: &ModRef,
    ) -> Result<DownloadResolution, DownloadCoordinatorError> {
        let context = InteractionContext {
            operation_id: plan.operation.operation_id.clone(),
            target_revision: plan.operation.fence.expected_content_revision,
            plan_hash: plan.operation.inputs_hash.clone(),
            module_lock_digest: provider.module_lock_digest.clone(),
            expires_at_ms: now_ms()?
                .checked_add(self.interaction_ttl_ms)
                .ok_or_else(|| {
                    DownloadCoordinatorError::InvalidPlan("interaction expiry overflow".to_owned())
                })?,
        };
        let resolution = provider
            .provider
            .begin_download(
                job_id,
                &context,
                game,
                mod_ref,
                &CancellationToken::default(),
            )
            .await?;
        if let DownloadResolution::Ready(descriptor) = &resolution {
            validate_descriptor(item, descriptor)?;
        }
        Ok(resolution)
    }

    async fn run_transfer_shared(
        &self,
        transfer_id: &str,
        request: &TransferRequest,
    ) -> Result<(), DownloadCoordinatorError> {
        let run_lock = {
            let mut runs = self
                .transfer_runs
                .lock()
                .map_err(|_| DownloadCoordinatorError::CatalogPoisoned)?;
            Arc::clone(
                runs.entry(transfer_id.to_owned())
                    .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
            )
        };
        let guard = run_lock.lock().await;
        if self.transfers.get(transfer_id)?.artifact.is_none() {
            self.transfers.run(transfer_id, request).await?;
        }
        drop(guard);
        if Arc::strong_count(&run_lock) == 2 {
            let mut runs = self
                .transfer_runs
                .lock()
                .map_err(|_| DownloadCoordinatorError::CatalogPoisoned)?;
            if runs
                .get(transfer_id)
                .is_some_and(|stored| Arc::ptr_eq(stored, &run_lock))
            {
                runs.remove(transfer_id);
            }
        }
        Ok(())
    }

    fn save_checkpoint(
        &self,
        job_id: &str,
        operation_id: &str,
        expected_seq: u64,
    ) -> Result<(), DownloadCoordinatorError> {
        self.jobs.save_checkpoint(
            job_id,
            expected_seq,
            &WorkerCheckpoint {
                step: "download-operation".to_owned(),
                payload: json!({"operationId": operation_id}),
                resume_policy: ResumePolicy::ResumeFromCheckpoint,
            },
        )?;
        Ok(())
    }

    fn checkpoint_cancel(
        &self,
        job_id: &str,
        operation_id: &str,
    ) -> Result<bool, DownloadCoordinatorError> {
        if self.is_cancelled(job_id, operation_id) {
            self.finish_cancel(job_id, operation_id)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn is_cancelled(&self, job_id: &str, operation_id: &str) -> bool {
        let operation_cancelled = self
            .library
            .download_operation(&self.authorization, operation_id)
            .is_ok_and(|operation| operation.state == DownloadOperationState::Cancelled);
        let job_cancelled = self
            .jobs
            .get_job(self.authorization.owner_scope(), job_id)
            .is_ok_and(|job| matches!(job.state, JobState::Cancelling | JobState::Cancelled));
        operation_cancelled || job_cancelled
    }

    fn finish_cancel(
        &self,
        job_id: &str,
        operation_id: &str,
    ) -> Result<(), DownloadCoordinatorError> {
        let operation = self
            .library
            .download_operation(&self.authorization, operation_id)?;
        if operation.state != DownloadOperationState::Cancelled {
            self.library
                .cancel_download_operation(&self.authorization, operation_id)?;
        }
        self.release_consumers(operation_id);
        self.jobs.cancel_at_safe_point(job_id)?;
        Ok(())
    }

    fn persist_failure(
        &self,
        job_id: &str,
        operation_id: &str,
        error: &DownloadCoordinatorError,
    ) -> Result<(), DownloadCoordinatorError> {
        let operation = self
            .library
            .download_operation(&self.authorization, operation_id)?;
        if !operation.state.is_terminal_for_coordinator() {
            self.library.fail_download_operation(
                &self.authorization,
                operation_id,
                operation.fence.fence_epoch,
                &operation.fence.owner_generation,
                error.code(),
                json!({"message": error.to_string()}),
            )?;
        }
        let job = self
            .jobs
            .get_job(self.authorization.owner_scope(), job_id)?;
        if !matches!(
            job.state,
            JobState::Succeeded | JobState::Cancelled | JobState::Failed
        ) {
            let mut update = EventUpdate::state("download-failed", JobState::Failed);
            update.error = Some(AppError {
                code: error.code().to_owned(),
                message_key: "download.failed".to_owned(),
                details: json!({"message": error.to_string(), "operationId": operation_id}),
                request_id: operation_id.to_owned(),
                job_id: Some(job_id.to_owned()),
                retryable: error.retryable(),
                recovery_actions: Vec::new(),
            });
            self.jobs.append_event(job_id, update)?;
        }
        Ok(())
    }

    fn track_consumer(
        &self,
        operation_id: &str,
        attachment: ConsumerAttachment,
    ) -> Result<(), DownloadCoordinatorError> {
        self.active_consumers
            .lock()
            .map_err(|_| DownloadCoordinatorError::CatalogPoisoned)?
            .entry(operation_id.to_owned())
            .or_default()
            .push(attachment);
        Ok(())
    }

    fn release_consumers(&self, operation_id: &str) {
        let attachments = self
            .active_consumers
            .lock()
            .ok()
            .and_then(|mut active| active.remove(operation_id))
            .unwrap_or_default();
        for attachment in attachments {
            let _ = self
                .transfers
                .detach_consumer(&attachment.transfer_id, &attachment.consumer_id);
        }
    }
}

trait CoordinatorOperationState {
    fn is_terminal_for_coordinator(&self) -> bool;
}

impl CoordinatorOperationState for DownloadOperationState {
    fn is_terminal_for_coordinator(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

fn item_mod_ref(item: &DownloadExecutionItem) -> ModRef {
    ModRef {
        source_id: item.source_id.clone(),
        remote_game_key: item.remote_game_key.clone(),
        remote_mod_id: item.source_mod_id.clone(),
        version_id: Some(item.resolved_version.clone()),
        remote_file_id: item.source_file_id.clone(),
    }
}

fn validate_descriptor(
    item: &DownloadExecutionItem,
    descriptor: &DownloadDescriptor,
) -> Result<(), DownloadCoordinatorError> {
    let reference = &descriptor.mod_ref;
    if reference.source_id != item.source_id
        || reference.remote_game_key != item.remote_game_key
        || reference.remote_mod_id != item.source_mod_id
        || reference.version_id.as_deref() != Some(item.resolved_version.as_str())
        || reference
            .remote_file_id
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        || item
            .source_file_id
            .as_deref()
            .is_some_and(|file_id| reference.remote_file_id.as_deref() != Some(file_id))
    {
        return Err(DownloadCoordinatorError::InvalidPlan(format!(
            "source descriptor changed fixed item {}",
            item.item_id
        )));
    }
    Ok(())
}

fn transfer_request(
    item: &DownloadExecutionItem,
    descriptor: DownloadDescriptor,
) -> Result<TransferRequest, DownloadCoordinatorError> {
    validate_descriptor(item, &descriptor)?;
    let remote_file_id = descriptor.mod_ref.remote_file_id.clone().ok_or_else(|| {
        DownloadCoordinatorError::InvalidPlan("missing remote file id".to_owned())
    })?;
    Ok(TransferRequest {
        file_key: TransferFileKey {
            source_id: item.source_id.clone(),
            remote_game_key: item.remote_game_key.clone(),
            remote_mod_id: item.source_mod_id.clone(),
            resolved_version_id: item.resolved_version.clone(),
            remote_file_id,
            byte_variant: item
                .validator
                .clone()
                .unwrap_or_else(|| "default".to_owned()),
        },
        sharing: TransferSharing::StableFileIdentity,
        descriptor,
        validation: TransferValidation {
            expected_sha256: item.expected_digest.clone(),
            required_prefix: Vec::new(),
            media_type: None,
        },
    })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn now_ms() -> Result<i64, DownloadCoordinatorError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| DownloadCoordinatorError::InvalidPlan(error.to_string()))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| DownloadCoordinatorError::InvalidPlan("timestamp overflow".to_owned()))
}

#[cfg(test)]
mod tests;
