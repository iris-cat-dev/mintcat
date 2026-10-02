use std::{collections::BTreeMap, sync::{Arc, Mutex, RwLock}};

use mintcat_feature_backup::MaintenanceGate;
use mintcat_game_api::{canonical_digest, GameDescriptor};
use mintcat_runtime_file_transactions::{FileCancellation, FileTransactionError};
use mintcat_runtime_jobs::{
    request_hash, ActionBinding, ActionSpec, ConfirmationPolicy, EventUpdate, JobStore,
    RecoveryDisposition, RecoveryRecord, RegisterJob, ResumePolicy, WorkerCheckpoint,
};
use mintcat_runtime_kernel::RuntimeLayout;
use mintcat_runtime_supervisor::ModuleSupervisor;
use mintcat_sdk::{ActionRequired, AppError, InstallApplyParams, InstallPlanParams, InstallUninstallParams, JobRef, JobState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    ApplyDisposition, ApprovalGrant, BuildRequest, ContentInputMetadata, GameAdapterRegistry,
    ImmutableInstallPlan, InstallError, InstallService, ModuleClosure, PlanInstallRequest,
    UnifiedUninstallDisposition, UninstallRequest, WorkerGameAdapter,
};

type GameVersions = BTreeMap<String, BTreeMap<String, Arc<WorkerGameAdapter>>>;

#[derive(Clone)]
pub struct GameCatalog {
    supervisor: ModuleSupervisor,
    adapters: GameAdapterRegistry,
    pinned: Arc<RwLock<GameVersions>>,
    current: Arc<RwLock<BTreeMap<String, Arc<WorkerGameAdapter>>>>,
    supervisors: Arc<RwLock<BTreeMap<String, ModuleSupervisor>>>,
}

impl GameCatalog {
    pub fn new(supervisor: ModuleSupervisor, adapters: GameAdapterRegistry) -> Self {
        Self { supervisor, adapters, pinned: Arc::default(), current: Arc::default(), supervisors: Arc::default() }
    }

    pub async fn pin_using(&self, supervisor: ModuleSupervisor, game_id: &str, game_digest: &str, closure: ModuleClosure) -> Result<Arc<WorkerGameAdapter>, InstallError> {
        if let Some(adapter) = self.pinned.read().expect("game catalog poisoned")
            .get(game_id).and_then(|versions| versions.get(&closure.digest)).cloned() {
            return Ok(adapter);
        }
        let adapter = WorkerGameAdapter::activate(supervisor.clone(), game_id, game_digest,
            closure.clone()).await
            .map_err(|message| InstallError::Adapter { game_id: game_id.to_owned(), message })?;
        let mut pinned = self.pinned.write().expect("game catalog poisoned");
        let versions = pinned.entry(game_id.to_owned()).or_default();
        if let Some(existing) = versions.get(&closure.digest) { return Ok(existing.clone()); }
        self.adapters.register(&closure, adapter.clone())?;
        self.supervisors.write().expect("game supervisors poisoned").insert(closure.digest.clone(), supervisor);
        versions.insert(closure.digest, adapter.clone());
        Ok(adapter)
    }

    /// Called only after scoped activation commits. Pinned historical versions
    /// remain usable by receipts and jobs, independently of this current view.
    pub fn publish(&self, adapter: Arc<WorkerGameAdapter>) {
        self.current.write().expect("game catalog poisoned")
            .insert(adapter.descriptor().game_id.clone(), adapter);
    }

    pub fn current(&self, game_id: &str) -> Result<Arc<WorkerGameAdapter>, InstallError> {
        self.current.read().expect("game catalog poisoned").get(game_id).cloned()
            .ok_or_else(|| InstallError::AdapterUnavailable(game_id.to_owned()))
    }

    pub fn descriptors(&self) -> Vec<GameDescriptor> {
        self.current.read().expect("game catalog poisoned").values()
            .map(|adapter| adapter.descriptor().clone()).collect()
    }

    async fn restore_closure(&self, request: &BuildRequest) -> Result<(), InstallError> {
        if self.pinned.read().expect("game catalog poisoned").get(&request.settings.game_id)
            .is_some_and(|versions| versions.contains_key(&request.module_closure.digest)) {
            return Ok(());
        }
        let mut game_digest = None;
        for entry in &request.module_closure.entries {
            let package = self.supervisor.artifacts().acquire_module(&entry.package_digest)?;
            if package.module().manifest.contributes.get("games").and_then(Value::as_array)
                .is_some_and(|games| games.iter().any(|game| game.get("gameId").and_then(Value::as_str)
                    == Some(request.settings.game_id.as_str()))) {
                if game_digest.replace(entry.package_digest.clone()).is_some() {
                    return Err(InstallError::InvalidRequest("closure has multiple adapters for one game".to_owned()));
                }
            }
        }
        let game_digest = game_digest.ok_or_else(|| InstallError::AdapterUnavailable(request.settings.game_id.clone()))?;
        self.pin_using(self.supervisor.clone(), &request.settings.game_id, &game_digest, request.module_closure.clone()).await?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), InstallError> {
        let supervisors = std::mem::take(&mut *self.supervisors.write().expect("game supervisors poisoned"));
        for supervisor in supervisors.into_values() {
            supervisor.shutdown_all().await.map_err(|error| InstallError::InvalidRequest(error.to_string()))?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildOptions {
    variant_id: String,
    settings_revision: u64,
    settings: Value,
    input_metadata: Vec<ContentInputMetadata>,
    #[serde(default)]
    expires_in_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase", deny_unknown_fields)]
enum InstallJobTarget {
    Build { request: BuildRequest, pack: bool, expires_in_ms: u64 },
    Apply { params: InstallApplyParams },
    Uninstall { request: UninstallRequest },
}

#[derive(Clone)]
pub struct InstallCoordinator {
    service: Arc<InstallService>,
    jobs: JobStore,
    catalog: GameCatalog,
    layout: RuntimeLayout,
    owner: String,
    running: Arc<Mutex<BTreeMap<String, FileCancellation>>>,
}

impl InstallCoordinator {
    pub fn new(service: InstallService, jobs: JobStore, catalog: GameCatalog, layout: RuntimeLayout, owner: impl Into<String>) -> Self {
        Self { service: Arc::new(service), jobs, catalog, layout, owner: owner.into(), running: Arc::default() }
    }
    pub fn catalog(&self) -> &GameCatalog { &self.catalog }
    pub fn service(&self) -> &InstallService { &self.service }

    pub fn submit_build(&self, params: InstallPlanParams, pack: bool, key: &str) -> Result<JobRef, InstallError> {
        let command = if pack { "mods.pack" } else { "install.plan" };
        let hash = request_hash(&serde_json::to_value(&params)?)?;
        if let Some(job) = self.existing(command, key, &hash)? { return Ok(job); }
        let options: BuildOptions = serde_json::from_value(params.options)?;
        let adapter = self.catalog.current(&params.game_id)?;
        let settings = adapter.settings_snapshot(options.settings_revision, options.settings)
            .map_err(|message| InstallError::Adapter { game_id: params.game_id, message })?;
        let request = BuildRequest {
            owner_scope: self.owner.clone(), profile_id: params.profile_id,
            expected_profile_revision: params.revision, expected_content_revision: params.content_revision,
            installation_id: params.installation_id, variant_id: options.variant_id,
            settings, module_closure: adapter.closure().clone(), input_metadata: options.input_metadata,
        };
        let target = InstallJobTarget::Build { request, pack, expires_in_ms: options.expires_in_ms.unwrap_or(3_600_000) };
        self.register(command, key, &hash, &target)
    }

    pub fn submit_apply(&self, params: InstallApplyParams, key: &str) -> Result<JobRef, InstallError> {
        let hash = request_hash(&serde_json::to_value(&params)?)?;
        if let Some(job) = self.existing("install.apply", key, &hash)? { return Ok(job); }
        let plan = self.service.plan(&params.plan_id)?;
        let action = self.jobs.get_action(&self.owner, &params.approval_ref)?;
        if action.kind != "install-approval" || action.plan_hash != params.plan_hash
            || action.target != install_target(&plan) || plan.plan_hash != params.plan_hash {
            return Err(InstallError::Approval("action is not bound to this install plan".to_owned()));
        }
        self.register("install.apply", key, &hash, &InstallJobTarget::Apply { params })
    }

    pub fn submit_uninstall(&self, params: InstallUninstallParams, key: &str) -> Result<JobRef, InstallError> {
        let hash = request_hash(&serde_json::to_value(&params)?)?;
        if let Some(job) = self.existing("install.uninstall", key, &hash)? { return Ok(job); }
        let request = UninstallRequest { installation_id: params.installation_id, receipt_id: params.receipt_id,
            expected_profile_revision: params.revision };
        self.service.uninstall_receipt(&request)?;
        self.register("install.uninstall", key, &hash, &InstallJobTarget::Uninstall { request })
    }

    pub fn cancel(&self, job_id: &str) -> Result<(), InstallError> {
        if let Some(cancellation) = self.running.lock().expect("install running map poisoned").get(job_id) {
            cancellation.request();
        } else {
            self.jobs.cancel_at_safe_point(job_id)?;
        }
        Ok(())
    }

    pub fn resume_action(&self, action: &ActionRequired) -> Result<(), InstallError> {
        if action.kind == "uninstall-approval" {
            let job = self.jobs.action_job(&self.owner, &action.action_id)?;
            let target: InstallJobTarget = serde_json::from_value(self.jobs.get_job_target(&self.owner, &job.job_id)?)?;
            if !matches!(target, InstallJobTarget::Uninstall { .. }) {
                return Err(InstallError::Approval("uninstall action has another job target".to_owned()));
            }
            self.spawn(job.job_id, Some(action.action_id.clone()));
        }
        Ok(())
    }

    pub async fn recover(&self, records: &[RecoveryRecord]) -> Result<(), InstallError> {
        for request in self.service.active_receipt_build_requests()? {
            if let Err(error) = self.catalog.restore_closure(&request).await {
                eprintln!("receipt closure {} remains fenced: {error}", request.module_closure.digest);
            }
        }
        let mut recoverable = Vec::new();
        for record in records.iter().filter(|record| is_install_command(&record.snapshot.command)) {
            let target = target_from_record(&self.jobs, &self.owner, &record.snapshot.job_id)?;
            let request = match target {
                InstallJobTarget::Build { request, .. } => request,
                InstallJobTarget::Apply { params } => self.service.recovery_build_request(&params.plan_id)?,
                InstallJobTarget::Uninstall { request } => self.service.recovery_build_request_for_receipt(&request.receipt_id)?,
            };
            if let Err(error) = self.catalog.restore_closure(&request).await {
                let mut event = EventUpdate::state("install-recovery-fenced",
                    if record.snapshot.state == JobState::AwaitingApproval {
                        JobState::AwaitingApproval
                    } else { JobState::RecoveryRequired });
                event.error = Some(install_app_error(&record.snapshot.job_id, &error));
                self.jobs.append_event(&record.snapshot.job_id, event)?;
                continue;
            }
            recoverable.push(record);
        }
        let service = self.service.clone();
        tokio::task::spawn_blocking(move || service.recover_file_transactions()).await
            .map_err(|error| InstallError::InvalidRequest(error.to_string()))??;
        for record in recoverable {
            if record.disposition == RecoveryDisposition::InspectCommit {
                let failure = install_app_error(&record.snapshot.job_id, &InstallError::InvalidRequest(
                    "interrupted commit was rolled back; submit a new job".to_owned(),
                ));
                self.reconcile_commit(&record.snapshot.job_id, failure, true)?;
            } else if record.disposition == RecoveryDisposition::ResumeCheckpoint {
                self.spawn(record.snapshot.job_id.clone(), None);
            }
        }
        self.service.release_recovered_content_guards()?;
        Ok(())
    }

    fn existing(&self, command: &str, key: &str, hash: &str) -> Result<Option<JobRef>, InstallError> {
        if self.jobs.find_operation(&self.owner, command, key)?.is_none() { return Ok(None); }
        Ok(Some(self.jobs.register_job(RegisterJob {
            command: command.to_owned(), owner_scope: self.owner.clone(), idempotency_key: key.to_owned(),
            request_hash: hash.to_owned(), target: Value::Null, confirmation_policy: ConfirmationPolicy::Required,
        })?.job))
    }

    fn register(&self, command: &str, key: &str, hash: &str, target: &InstallJobTarget) -> Result<JobRef, InstallError> {
        let registration = self.jobs.register_job(RegisterJob {
            command: command.to_owned(), owner_scope: self.owner.clone(), idempotency_key: key.to_owned(),
            request_hash: hash.to_owned(), target: serde_json::to_value(target)?,
            confirmation_policy: if command == "mods.pack" { ConfirmationPolicy::None } else { ConfirmationPolicy::Required },
        })?;
        if registration.created {
            let snapshot = self.jobs.get_job(&self.owner, &registration.job.job_id)?;
            self.jobs.save_checkpoint(&registration.job.job_id, snapshot.last_seq, &WorkerCheckpoint {
                step: "install-inputs-pinned".to_owned(), payload: serde_json::to_value(target)?,
                resume_policy: ResumePolicy::ResumeFromCheckpoint,
            })?;
            self.spawn(registration.job.job_id.clone(), None);
        }
        Ok(registration.job)
    }

    fn spawn(&self, job_id: String, action_id: Option<String>) {
        let cancellation = FileCancellation::default();
        let mut running = self.running.lock().expect("install running map poisoned");
        if running.contains_key(&job_id) { return; }
        running.insert(job_id.clone(), cancellation.clone());
        drop(running);
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || {
            let result = coordinator.execute(&job_id, action_id.as_deref(), &cancellation);
            if let Err(error) = result { coordinator.record_failure(&job_id, &error); }
            coordinator.running.lock().expect("install running map poisoned").remove(&job_id);
        });
    }

    fn execute(&self, job_id: &str, action_id: Option<&str>, cancellation: &FileCancellation) -> Result<(), InstallError> {
        let _admission = MaintenanceGate::enter_mutation(&self.layout)
            .map_err(|error| InstallError::InvalidRequest(error.to_string()))?;
        if self.jobs.cancellation_requested(&self.owner, job_id)? { cancellation.request(); }
        if cancellation.is_requested() { self.jobs.cancel_at_safe_point(job_id)?; return Ok(()); }
        let target = target_from_record(&self.jobs, &self.owner, job_id)?;
        if action_id.is_none() {
            self.jobs.append_event(job_id, EventUpdate::state("install-preparing", JobState::Preparing))?;
            self.jobs.append_event(job_id, EventUpdate::state("install-running", JobState::Running))?;
        }
        match target {
            InstallJobTarget::Build { request, pack, expires_in_ms } => {
                if pack {
                    let result = self.service.pack(request, cancellation)?;
                    if cancellation.is_requested() { self.jobs.cancel_at_safe_point(job_id)?; return Ok(()); }
                    let mut event = EventUpdate::state("pack-verified", JobState::Succeeded);
                    event.result = Some(serde_json::to_value(result)?);
                    self.jobs.append_event(job_id, event)?;
                } else {
                    let plan = self.service.plan_install(PlanInstallRequest { build: request, expires_in_ms }, cancellation)?;
                    let mut result = serde_json::to_value(&plan)?;
                    let action = self.jobs.create_action(job_id, ActionSpec {
                        kind: "install-approval".to_owned(), operation_id: plan.plan_id.clone(),
                        target: install_target(&plan), target_revision: plan.profile.profile_revision,
                        plan_hash: plan.plan_hash, module_lock_digest: plan.module_closure.digest,
                        expires_at_ms: plan.expires_at_ms, response_schema: approval_schema(),
                    })?;
                    result.as_object_mut().expect("serialized plan is an object")
                        .insert("actionId".to_owned(), Value::String(action.action_id));
                    let mut event = EventUpdate::state("install-plan-verified", JobState::AwaitingApproval);
                    event.result = Some(result);
                    self.jobs.append_event(job_id, event)?;
                }
            }
            InstallJobTarget::Apply { params } => {
                let plan = self.service.plan(&params.plan_id)?;
                let consumed = self.jobs.consume_action(&params.approval_ref, &install_binding(&plan))?;
                if consumed.response.get("approved") != Some(&Value::Bool(true)) {
                    return Err(InstallError::Approval("human action did not approve the plan".to_owned()));
                }
                if self.jobs.get_job(&self.owner, &consumed.job.job_id)?.state != JobState::Succeeded {
                    let mut approved = EventUpdate::state("plan-approved", JobState::Succeeded);
                    approved.details = Some(json!({"applyJobId":job_id}));
                    self.jobs.append_event(&consumed.job.job_id, approved)?;
                }
                let snapshot = self.jobs.get_job(&self.owner, job_id)?;
                self.jobs.save_checkpoint(job_id, snapshot.last_seq, &WorkerCheckpoint {
                    step: "install-before-commit".to_owned(), payload: serde_json::to_value(&params)?,
                    resume_policy: ResumePolicy::InspectBeforeResume,
                })?;
                self.jobs.begin_commit(job_id)?;
                let result = self.service.apply(&params.plan_id, &params.plan_hash, &ApprovalGrant {
                    approval_ref: params.approval_ref, plan_id: plan.plan_id, plan_hash: plan.plan_hash,
                    approved_risks: plan.required_approvals, expires_at_ms: plan.expires_at_ms,
                }, cancellation)?;
                if result.disposition == ApplyDisposition::Cancelled {
                    self.jobs.finish_uncommitted(job_id, None)?;
                } else { self.jobs.finish_commit(job_id, serde_json::to_value(result)?)?; }
            }
            InstallJobTarget::Uninstall { request } => {
                let receipt = self.service.uninstall_receipt(&request)?;
                let target = json!({"installationId":request.installation_id,"receiptId":request.receipt_id,"rootPath":receipt.root_path});
                let binding = ActionBinding { target, target_revision: request.expected_profile_revision,
                    plan_hash: canonical_digest(&receipt)?, module_lock_digest: receipt.module_lock_digest.clone() };
                if let Some(action_id) = action_id {
                    self.jobs.consume_action(action_id, &binding)?;
                    let snapshot = self.jobs.get_job(&self.owner, job_id)?;
                    self.jobs.save_checkpoint(job_id, snapshot.last_seq, &WorkerCheckpoint {
                        step: "uninstall-before-commit".to_owned(), payload: serde_json::to_value(&request)?,
                        resume_policy: ResumePolicy::InspectBeforeResume,
                    })?;
                    self.jobs.begin_commit(job_id)?;
                    let result = self.service.uninstall(&request, cancellation)?;
                    if result.disposition == UnifiedUninstallDisposition::Cancelled {
                        self.jobs.finish_uncommitted(job_id, None)?;
                    } else { self.jobs.finish_commit(job_id, serde_json::to_value(result)?)?; }
                } else {
                    self.jobs.create_action(job_id, ActionSpec {
                        kind: "uninstall-approval".to_owned(), operation_id: request.receipt_id,
                        target: binding.target, target_revision: binding.target_revision,
                        plan_hash: binding.plan_hash, module_lock_digest: binding.module_lock_digest,
                        expires_at_ms: crate::service::now_ms()? + 3_600_000, response_schema: approval_schema(),
                    })?;
                }
            }
        }
        Ok(())
    }

    fn record_failure(&self, job_id: &str, error: &InstallError) {
        let app_error = install_app_error(job_id, error);
        let persisted = match self.jobs.commit_state(&self.owner, job_id) {
            Ok(mintcat_runtime_jobs::CommitState::InProgress | mintcat_runtime_jobs::CommitState::Unknown) =>
                self.reconcile_commit(job_id, app_error, false),
            _ => match self.jobs.cancellation_requested(&self.owner, job_id) {
                Ok(true) => self.jobs.cancel_at_safe_point(job_id).map_err(InstallError::from),
                _ => {
                    let mut update = EventUpdate::state("install-failed", JobState::Failed);
                    update.error = Some(app_error);
                    self.jobs.append_event(job_id, update).map_err(InstallError::from)
                }
            },
        };
        if let Err(persistence_error) = persisted {
            eprintln!("install job {job_id} failure could not be persisted: {persistence_error}; cause: {error}");
        }
    }

    fn reconcile_commit(
        &self,
        job_id: &str,
        failure: AppError,
        recovered: bool,
    ) -> Result<mintcat_sdk::JobSnapshot, InstallError> {
        let target = match target_from_record(&self.jobs, &self.owner, job_id) {
            Ok(target) => target,
            Err(_) => return Ok(self.jobs.mark_commit_unknown(job_id)?),
        };
        let inspected: Result<(Option<Value>, bool), InstallError> = match target {
            InstallJobTarget::Apply { params } => self.service.committed_result(&params.plan_id)
                .and_then(|result| Ok((result.map(serde_json::to_value).transpose()?,
                    self.service.has_unresolved_plan(&params.plan_id)?))),
            InstallJobTarget::Uninstall { request } => self.service.committed_uninstall_result(&request)
                .and_then(|result| Ok((result.map(serde_json::to_value).transpose()?,
                    self.service.has_unresolved_uninstall(&request.installation_id)?))),
            InstallJobTarget::Build { .. } => return Ok(self.jobs.mark_commit_unknown(job_id)?),
        };
        match inspected {
            Ok((Some(result), _)) => Ok(if recovered {
                self.jobs.finish_recovered_commit(job_id, result)?
            } else { self.jobs.finish_commit(job_id, result)? }),
            Ok((None, false)) => Ok(self.jobs.finish_uncommitted(job_id, Some(failure))?),
            Ok((None, true)) | Err(_) => Ok(self.jobs.mark_commit_unknown(job_id)?),
        }
    }
}

fn target_from_record(jobs: &JobStore, owner: &str, job_id: &str) -> Result<InstallJobTarget, InstallError> {
    Ok(serde_json::from_value(jobs.get_job_target(owner, job_id)?)?)
}
fn install_target(plan: &ImmutableInstallPlan) -> Value {
    json!({"gameId":plan.profile.game_id,"profileId":plan.profile.profile_id,
        "installationId":plan.profile.installation_id,"planId":plan.plan_id})
}
fn install_binding(plan: &ImmutableInstallPlan) -> ActionBinding {
    ActionBinding { target: install_target(plan), target_revision: plan.profile.profile_revision,
        plan_hash: plan.plan_hash.clone(), module_lock_digest: plan.module_closure.digest.clone() }
}
fn approval_schema() -> Value {
    json!({"type":"object","required":["approved"],"additionalProperties":false,
        "properties":{"approved":{"type":"boolean","const":true}}})
}
fn is_install_command(command: &str) -> bool {
    matches!(command, "install.plan" | "install.apply" | "install.uninstall" | "mods.pack")
}
pub fn install_app_error(job_id: &str, error: &InstallError) -> AppError {
    let code = match error {
        InstallError::AdapterUnavailable(_) => "MODULE_UNAVAILABLE",
        InstallError::ProfileChanged { .. } => "REVISION_CONFLICT",
        InstallError::PlanNotFound(_) => "NOT_FOUND",
        InstallError::Approval(_) | InstallError::PlanExpired(_) => "STALE_PLAN",
        InstallError::FileTransaction(FileTransactionError::RootBusy(_)) => "RESOURCE_BUSY",
        InstallError::FileTransaction(FileTransactionError::TargetRunning(_)) => "GAME_RUNNING",
        InstallError::FileTransaction(FileTransactionError::RecoveryRequired { .. }) => "RECOVERY_REQUIRED",
        _ => "INSTALL_FAILED",
    };
    AppError { code: code.to_owned(), message_key: "install.operation.failed".to_owned(),
        details: json!({"reason":error.to_string()}), request_id: job_id.to_owned(), job_id: Some(job_id.to_owned()),
        retryable: false, recovery_actions: Vec::new() }
}
