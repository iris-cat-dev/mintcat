use std::{collections::HashMap, sync::{Arc, Mutex, RwLock, Weak}};

use async_trait::async_trait;
use mintcat_feature_agent::{AgentError, NativeAgentSessions, NativeAgentStateOwner, ApplicationGateway, CreateSession, SendMessage};
use mintcat_feature_backup::{MaintenanceAdmission, MaintenanceGate};
use mintcat_feature_proxy::{PrivilegeOperation, ProxyServiceManager, StartOptions, MODULE_ID};
use mintcat_runtime_artifacts::ArtifactStore;
use mintcat_runtime_broker::NetworkBroker;
use mintcat_runtime_jobs::{request_hash, ConfirmationPolicy, EventUpdate, JobStore, RecoveryRecord, RegisterJob};
use mintcat_runtime_kernel::RuntimeLayout;
use mintcat_sdk::{AppError, JobState, ValidatedRequest};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::task::JoinHandle;

use super::{DaemonState, Principal, LOCAL_OWNER_SCOPE};

type Result<T, E = Box<AppError>> = std::result::Result<T, E>;

struct AgentGateway { state: RwLock<Weak<DaemonState>>, executor: Mutex<Option<tokio::runtime::Handle>> }
#[async_trait]
impl ApplicationGateway for AgentGateway {
    async fn execute(&self, request: ValidatedRequest) -> std::result::Result<Value, AppError> {
        let state = self.state.read().map_err(|_| *error("INTERNAL", "agent.gateway", "gateway"))?.upgrade()
            .ok_or_else(|| *error("MODULE_UNAVAILABLE", "agent.gateway", "gateway"))?;
        // Principal authority is supplied here, never by model or caller parameters.
        let executor = self.executor.lock().map_err(|_| *error("INTERNAL", "agent.executor", "gateway"))?.clone()
            .ok_or_else(|| *error("MODULE_UNAVAILABLE", "agent.executor", "gateway"))?;
        executor.spawn(async move { Box::pin(super::application::handle_request(state, request, Principal::Agent)).await }).await
            .map_err(|_| *error("INTERNAL", "agent.executor", "gateway"))?.map_err(|error| *error)
    }
}

struct Worker { handle: JoinHandle<()>, agent_session: Option<String>, privilege_execution: bool }
pub(crate) struct FeatureRuntime {
    jobs: JobStore,
    agent: Arc<NativeAgentSessions>,
    gateway: Arc<AgentGateway>,
    proxy: tokio::sync::Mutex<ProxyServiceManager>,
    workers: Mutex<HashMap<String, Worker>>,
    proxy_data: std::path::PathBuf,
    service_lease: Mutex<Option<(mintcat_runtime_updates::UpdatesCoordinator, String)>>,
    agent_backups: std::path::PathBuf,
    proxy_writes: std::sync::atomic::AtomicBool,
    recovered_actions: Mutex<Vec<(String, String)>>,
}
impl FeatureRuntime {
    pub(crate) fn new(layout: &RuntimeLayout, artifacts: ArtifactStore, jobs: JobStore, broker: NetworkBroker) -> Result<Self> {
        let data = layout.state_dir.join("features");
        std::fs::create_dir_all(&data).map_err(|_| error("INTERNAL", "features.storage", "startup"))?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).map_err(|_| error("INTERNAL", "features.storage", "startup"))?;
        }
        let gateway = Arc::new(AgentGateway { state: RwLock::new(Weak::new()), executor: Mutex::new(None) });
        let agent = Arc::new(NativeAgentSessions::unavailable(&data.join("agent.sqlite3"), broker, gateway.clone()));
        let proxy_data = data.join("proxy");
        let proxy = ProxyServiceManager::new_for_owner(artifacts, jobs.clone(), proxy_data.clone(), LOCAL_OWNER_SCOPE)
            .map_err(|_| error("INTERNAL", "proxy.storage", "startup"))?;
        Ok(Self { jobs, agent, gateway, proxy: tokio::sync::Mutex::new(proxy), workers: Mutex::new(HashMap::new()), proxy_data, service_lease: Mutex::new(None), agent_backups: data.join("agent-backups"), proxy_writes: std::sync::atomic::AtomicBool::new(true), recovered_actions: Mutex::new(Vec::new()) })
    }
    pub(crate) fn bind_state(&self, state: &Arc<DaemonState>) -> Result<()> {
        let mut binding = self.gateway.state.write().map_err(|_| error("INTERNAL", "agent.gateway", "startup"))?;
        if !Weak::ptr_eq(&binding, &Weak::new()) { return Err(error("CONFLICT", "agent.gateway_already_bound", "startup")); }
        *binding = Arc::downgrade(state);
        Ok(())
    }
    pub(crate) fn bind_executor(&self) -> Result<()> {
        let mut executor = self.gateway.executor.lock().map_err(|_| error("INTERNAL", "agent.executor", "startup"))?;
        if executor.is_none() {
            *executor = Some(tokio::runtime::Handle::try_current().map_err(|_| error("MODULE_UNAVAILABLE", "agent.executor", "startup"))?);
        }
        Ok(())
    }
    pub(crate) fn agent_state_owner(&self) -> Arc<dyn mintcat_runtime_updates::StateOwner> {
        Arc::new(NativeAgentStateOwner::new(self.agent.clone(), self.agent_backups.clone()))
    }
    pub(crate) fn proxy_state_owner(self: &Arc<Self>) -> Arc<dyn mintcat_runtime_updates::StateOwner> {
        Arc::new(ProxyStateOwner { runtime: Arc::downgrade(self), selected: Mutex::new(None), backups: self.proxy_data.join("owner-backups") })
    }
    fn state(&self) -> Result<Arc<DaemonState>> {
        self.gateway.state.read().map_err(|_| error("INTERNAL", "agent.gateway", "features"))?.upgrade()
            .ok_or_else(|| error("MODULE_UNAVAILABLE", "agent.gateway", "features"))
    }
    pub(crate) fn portable_epochs(&self) -> Result<Value> {
        let agent=if self.agent.has_native().map_err(|e|agent_error(e,"backup"))? {
            self.agent.write_epoch().map_err(|e|agent_error(e,"backup"))?
        } else if self.agent.is_pristine_absent().map_err(|e|agent_error(e,"backup"))? {0}
        else {return Err(error("MODULE_UNAVAILABLE","backup.agent_owner","backup"));};
        let proxy=self.proxy.try_lock().map_err(|_|error("RESOURCE_BUSY","backup.proxy_owner","backup"))?;
        Ok(json!({"mintcat.feature.agent":agent,"mintcat.feature.proxy":proxy.write_epoch()}))
    }
    fn proxy_portable_owner_available(&self) -> std::result::Result<bool, mintcat_feature_backup::BackupError> {
        use mintcat_feature_backup::BackupError;
        let state = self.state().map_err(|error| BackupError::Module(format!("{}: {}", error.code, error.message_key)))?;
        let updates = state.updates.as_ref().ok_or_else(|| BackupError::Module("signed module closure unavailable".into()))?;
        Ok(updates.environment().map_err(|error| BackupError::Module(error.to_string()))?.scopes.values()
            .any(|scope| scope.targets.signed.packages.values().any(|package| package.manifest.id == MODULE_ID)))
    }
    fn track(&self, job_id: String, worker: Worker) -> Result<()> {
        let mut workers = self.workers.lock().map_err(|_| error("INTERNAL", "features.workers", &job_id))?;
        workers.retain(|_, worker| !worker.handle.is_finished());
        workers.insert(job_id, worker);
        Ok(())
    }
    pub(crate) async fn cancel(&self, job_id: &str) -> Result<bool> {
        let snapshot = self.jobs.get_job(LOCAL_OWNER_SCOPE, job_id).map_err(|e| job_error(e, job_id))?;
        if !owned_command(&snapshot.command) { return Ok(false); }
        let worker = self.workers.lock().map_err(|_| error("INTERNAL", "features.workers", job_id))?.remove(job_id);
        if let Some(worker) = worker {
            // Interrupt the native provider, then wait while its admission remains held.
            if let Some(session_id) = &worker.agent_session {
                let session = session_id.clone(); let job = job_id.to_owned();
                agent_call(self, move |agent| agent.interrupt_turn(&session, &job), job_id).await?;
            }
            let _ = worker.handle.await;
        }
        self.jobs.cancel_at_safe_point(job_id).map_err(|e| job_error(e, job_id))?;
        self.release_rejected_privilege(job_id)?;
        Ok(true)
    }
    pub(crate) async fn resume_action(self: &Arc<Self>, action_id: &str) -> Result<bool> {
        let action = self.jobs.get_action(LOCAL_OWNER_SCOPE, action_id).map_err(|e| job_error(e, action_id))?;
        if action.kind != "proxy-system-operation" { return Ok(false); }
        let action_job_id = action.operation_id.strip_prefix("proxy:").ok_or_else(|| error("PERMISSION_DENIED", "proxy.action_authority", action_id))?;
        let job = self.jobs.get_job(LOCAL_OWNER_SCOPE, action_job_id).map_err(|e| job_error(e, action_id))?;
        if job.command != "proxy.privilege" || action.target.get("moduleId") != Some(&json!(MODULE_ID)) {
            return Err(error("PERMISSION_DENIED", "proxy.action_authority", action_id));
        }
        if job.state == JobState::Succeeded { return Ok(true); }
        let preparation = {
            let mut workers = self.workers.lock().map_err(|_| error("INTERNAL", "features.workers", action_id))?;
            if workers.get(&job.job_id).is_some_and(|worker| worker.privilege_execution && !worker.handle.is_finished()) { return Ok(true); }
            workers.remove(&job.job_id)
        };
        // Approval can arrive before its planning worker finishes lease persistence.
        if let Some(preparation) = preparation { let _ = preparation.handle.await; }
        if self.workers.lock().map_err(|_| error("INTERNAL", "features.workers", action_id))?.get(&job.job_id).is_some_and(|worker| worker.privilege_execution && !worker.handle.is_finished()) { return Ok(true); }
        if !matches!(self.jobs.get_job(LOCAL_OWNER_SCOPE, &job.job_id).map_err(|e| job_error(e, action_id))?.state, JobState::AwaitingApproval | JobState::Running) {
            return Err(error("STALE_PLAN", "proxy.action_job_not_ready", action_id));
        }
        if action.state != mintcat_sdk::ActionState::Approved { return Err(error("APPROVAL_REQUIRED", "proxy.approval_required", action_id)); }
        let digest = action.target.get("digest").and_then(Value::as_str).ok_or_else(|| error("STALE_PLAN", "proxy.action_authority", action_id))?.to_owned();
        let operation: PrivilegeOperation = serde_json::from_value(action.target.get("operation").cloned().ok_or_else(|| error("STALE_PLAN", "proxy.action_authority", action_id))?)
            .map_err(|_| error("STALE_PLAN", "proxy.action_authority", action_id))?;
        let state = self.state()?;
        let admission = admission(&state, MODULE_ID, cleanup(&operation), action_id)?;
        let artifacts = state.updates.as_ref().map(|updates| updates.artifact_store_for_digest(&digest)).transpose()
            .map_err(|_| error("ARTIFACT_REVOKED", "proxy.artifact_authority", action_id))?;
        let runtime = Arc::clone(self);
        let action_id = action_id.to_owned();
        let job_id = job.job_id.clone();
        let handle = tokio::task::spawn_blocking(move || {
            let _admission = admission;
            let mut proxy = runtime.proxy.blocking_lock();
            if let Some(artifacts) = artifacts { proxy.set_artifact_store(artifacts); }
            if let Err(failure) = proxy.execute_privilege(&action_id, &digest, operation) {
                // execute_privilege itself marks any uncertain system commit recovery-required.
                if !matches!(runtime.jobs.get_job(LOCAL_OWNER_SCOPE, &job.job_id).map(|job| job.state), Ok(JobState::RecoveryRequired)) {
                    runtime.fail(&job.job_id, proxy_error(&failure, "SYSTEM_AUTHORIZATION_REQUIRED", &action_id));
                }
            }
        });
        self.track(job_id, Worker { handle, agent_session: None, privilege_execution: true })?;
        Ok(true)
    }
    pub(crate) fn recover(&self, records: &[RecoveryRecord]) -> Result<()> {
        for record in records.iter().filter(|record| owned_command(&record.snapshot.command)) {
            // A recovered provider tool turn or native/system side effect is not safe to repeat.
            // Retain unknown proxy commits for explicit inspection; never invent service readiness.
            if record.snapshot.command == "agent.sessions.send" || matches!(record.snapshot.command.as_str(), "proxy.start" | "proxy.stop") {
                self.fail(&record.snapshot.job_id, error("TURN_INTERRUPTED", "features.interrupted", &record.snapshot.job_id));
            }
            if record.snapshot.command == "proxy.privilege" && record.snapshot.state == JobState::AwaitingApproval {
                if let Some(action) = self.jobs.get_job_action(LOCAL_OWNER_SCOPE, &record.snapshot.job_id).map_err(|failure| job_error(failure, &record.snapshot.job_id))? {
                    if action.state == mintcat_sdk::ActionState::Approved && action.kind == "proxy-system-operation" && action.target.get("moduleId") == Some(&json!(MODULE_ID)) {
                        self.recovered_actions.lock().map_err(|_| error("INTERNAL", "proxy.action_recovery", "recovery"))?.push((record.snapshot.job_id.clone(), action.action_id));
                    }
                }
            }
        }
        if let Some(updates) = self.state()?.updates.as_ref() {
            // The manager kills/reaps every owned child on daemon loss; no prior process survives
            // as this daemon's stop controller. Release only our exact durable service owners.
            for lease in updates.leases().map_err(|_| error("INTERNAL", "proxy.lease_recovery", "recovery"))?
                .into_iter().filter(|lease| lease.kind == mintcat_runtime_updates::LeaseKind::Service && lease.owner_id.starts_with("proxy-native-service:")) {
                updates.release_lease(&lease.lease_id).map_err(|_| error("INTERNAL", "proxy.lease_recovery", "recovery"))?;
            }
        }
        Ok(())
    }
    pub(crate) async fn resume_recovered(self: &Arc<Self>) -> Result<()> {
        let actions = std::mem::take(&mut *self.recovered_actions.lock().map_err(|_| error("INTERNAL", "proxy.action_recovery", "recovery"))?);
        for (job_id, action_id) in actions {
            if let Err(failure) = self.resume_action(&action_id).await { self.fail(&job_id, failure); }
        }
        Ok(())
    }
    pub(crate) async fn shutdown(&self) -> Result<()> {
        let workers = std::mem::take(&mut *self.workers.lock().map_err(|_| error("INTERNAL", "features.workers", "shutdown"))?);
        for (job_id, worker) in workers {
            if let Some(session_id) = &worker.agent_session {
                let session = session_id.clone(); let job = job_id.clone();
                agent_call(self, move |agent| agent.interrupt_turn(&session, &job), &job_id).await?;
            }
            let _ = worker.handle.await;
            if worker.agent_session.is_some() {
                let state = self.jobs.get_job(LOCAL_OWNER_SCOPE, &job_id).map_err(|e| job_error(e, &job_id))?.state;
                if !matches!(state, JobState::Succeeded | JobState::Failed | JobState::Cancelled) { self.fail(&job_id, error("TURN_INTERRUPTED", "agent.interrupted", &job_id)); }
            }
        }
        self.proxy.lock().await.stop().await.map_err(|_| error("SERVICE_STOP_FAILED", "proxy.stop_failed", "shutdown"))?;
        self.release_service_lease()?;
        agent_call(self, |agent| agent.shutdown(), "shutdown").await?;
        Ok(())
    }
    fn fail(&self, job_id: &str, mut failure: Box<AppError>) {
        if let Ok(snapshot) = self.jobs.cancel_at_safe_point(job_id) {
            if matches!(snapshot.state, JobState::Cancelled | JobState::Succeeded | JobState::Failed) {
                if let Err(failure) = self.release_rejected_privilege(job_id) { eprintln!("feature rejected lease release failed for {job_id}: {}", failure.code); }
                return;
            }
        }
        failure.job_id = Some(job_id.into());
        let mut event = EventUpdate::state("feature-failed", JobState::Failed);
        event.error = Some(*failure);
        if let Err(error) = self.jobs.append_event(job_id, event) { eprintln!("feature terminal persistence failed for {job_id}: {error}"); }
        if let Err(failure) = self.release_rejected_privilege(job_id) { eprintln!("feature rejected lease release failed for {job_id}: {}", failure.code); }
    }
    fn succeeded(&self, job_id: &str, result: Value) {
        let mut event = EventUpdate::state("feature-complete", JobState::Succeeded);
        event.result = Some(result);
        if let Err(error) = self.jobs.append_event(job_id, event) { eprintln!("feature terminal persistence failed for {job_id}: {error}"); }
    }
    fn release_service_lease(&self) -> Result<()> {
        let mut lease = self.service_lease.lock().map_err(|_| error("INTERNAL", "proxy.lease", "proxy"))?;
        if let Some((updates, lease_id)) = lease.as_ref() {
            updates.release_lease(lease_id).map_err(|_| error("INTERNAL", "proxy.lease_release", "proxy"))?;
            *lease = None;
        }
        Ok(())
    }
    fn release_rejected_privilege(&self, job_id: &str) -> Result<()> {
        let snapshot = self.jobs.get_job(LOCAL_OWNER_SCOPE, job_id).map_err(|failure| job_error(failure, job_id))?;
        if snapshot.command != "proxy.privilege" || !matches!(snapshot.state, JobState::Cancelled | JobState::Failed) { return Ok(()); }
        if let Some(updates) = self.state()?.updates.as_ref() {
            let owner = format!("proxy-native-privilege:{job_id}");
            for lease in updates.leases().map_err(|_| error("INTERNAL", "proxy.lease_read", job_id))?.into_iter().filter(|lease| lease.owner_id == owner && lease.kind == mintcat_runtime_updates::LeaseKind::Task) {
                updates.release_lease(&lease.lease_id).map_err(|_| error("INTERNAL", "proxy.lease_release", job_id))?;
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionLookup { session_id: String }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionDelete { session_id: String, expected_revision: u64 }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProxyStart { digest: String, options: StartOptions }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProxyPrivilege { digest: String, operation: PrivilegeOperation }

pub(crate) async fn handle(state: &Arc<DaemonState>, request: &ValidatedRequest) -> Option<Result<Value>> {
    if !matches!(request.envelope.method.as_str(), "agent.sessions.create" | "agent.sessions.get" | "agent.sessions.send" | "agent.sessions.delete" | "proxy.status" | "proxy.start" | "proxy.stop" | "proxy.privilege") { return None; }
    Some(route(state, request).await)
}
async fn route(state: &Arc<DaemonState>, request: &ValidatedRequest) -> Result<Value> {
    let runtime = &state.features;
    runtime.bind_executor()?;
    let id = super::rpc_id_string(&request.envelope.id);
    let params = &request.envelope.params;
    let key = request.envelope.idempotency_key.as_deref().unwrap_or("");
    if owned_command(&request.envelope.method) && runtime.jobs.find_operation(LOCAL_OWNER_SCOPE, &request.envelope.method, key).map_err(|e| job_error(e, &id))?.is_some() {
        let replay = register(runtime, request, Value::Null, ConfirmationPolicy::None, &id)?;
        return encode(replay.job, &id);
    }
    match request.envelope.method.as_str() {
        "agent.sessions.get" => {
            let lookup: SessionLookup = decode(params, &id)?;
            encode(agent_call(runtime, move |agent| agent.get(&lookup.session_id), &id).await?, &id)
        }
        "agent.sessions.create" => {
            let input: CreateSession = decode(params, &id)?;
            let hash = request_hash(params).map_err(|e| job_error(e, &id))?;
            let _admission = admission(state, "mintcat.feature.agent", false, &id)?;
            let key = key.to_owned();
            encode(agent_call(runtime, move |agent| agent.create_keyed(input, &key, &hash), &id).await?, &id)
        }
        "agent.sessions.delete" => {
            let input: SessionDelete = decode(params, &id)?;
            let hash = request_hash(params).map_err(|e| job_error(e, &id))?;
            let _admission = admission(state, "mintcat.feature.agent", false, &id)?;
            let key = key.to_owned();
            agent_call(runtime, move |agent| agent.delete_keyed(&input.session_id, input.expected_revision, &key, &hash), &id).await?;
            Ok(json!({"deleted":true}))
        }
        "agent.sessions.send" => {
            let input: SendMessage = decode(params, &id)?;
            let admission = admission(state, "mintcat.feature.agent", false, &id)?;
            let registered = register(runtime, request, json!({"sessionId":input.session_id}), ConfirmationPolicy::None, &id)?;
            if registered.created {
                let session_id = input.session_id.clone();
                let prepare_job_id = registered.job.job_id.clone();
                if let Err(failure) = agent_call(runtime, move |agent| agent.prepare_send(input, &prepare_job_id), &id).await {
                    runtime.fail(&registered.job.job_id, failure.clone());
                    return Err(failure);
                }
                if let Err(failure) = preparing(&runtime.jobs, &registered.job.job_id, &id) {
                    let session = session_id.clone(); let job = registered.job.job_id.clone();
                    agent_call(runtime, move |agent| agent.interrupt_turn(&session, &job), &id).await?;
                    runtime.fail(&registered.job.job_id, failure.clone());
                    return Err(failure);
                }
                let runtime_worker = Arc::clone(runtime);
                let job_id = registered.job.job_id.clone();
                let tracked_session = session_id.clone();
                let generation = state.generation_id.clone();
                let handle = tokio::spawn(async move {
                    let _admission = admission;
                    if let Err(failure) = running(&runtime_worker.jobs, &job_id, &job_id) {
                        let session = session_id.clone(); let job = job_id.clone();
                        if let Err(interrupt) = agent_call(&runtime_worker, move |agent| agent.interrupt_turn(&session, &job), &job_id).await { runtime_worker.fail(&job_id, interrupt); }
                        runtime_worker.fail(&job_id, failure);
                        return;
                    }
                    match runtime_worker.agent.run_turn(&session_id, &job_id, &generation).await {
                        Ok(session) => runtime_worker.succeeded(&job_id, json!({"sessionId":session.session_id,"revision":session.revision})),
                        Err(failure) => runtime_worker.fail(&job_id, agent_error(failure, &job_id)),
                    }
                });
                runtime.track(registered.job.job_id.clone(), Worker { handle, agent_session: Some(tracked_session), privilege_execution: false })?;
            }
            encode(registered.job, &id)
        }
        "proxy.status" => {
            // Failed-health cleanup can mutate owned hosts; keep maintenance admission.
            let _admission = admission(state, MODULE_ID, true, &id)?;
            encode(runtime.proxy.lock().await.health().await.map_err(|e| proxy_error(&e, "SERVICE_UNHEALTHY", &id))?, &id)
        }
        method => {
            let start: Option<ProxyStart> = if method == "proxy.start" { Some(decode(params, &id)?) } else { None };
            let privilege: Option<ProxyPrivilege> = if method == "proxy.privilege" { Some(decode(params, &id)?) } else { None };
            let cleanup_only = method == "proxy.stop" || privilege.as_ref().is_some_and(|input| cleanup(&input.operation));
            let admission = admission(state, MODULE_ID, cleanup_only, &id)?;
            let digest = start.as_ref().map(|input| input.digest.as_str()).or_else(|| privilege.as_ref().map(|input| input.digest.as_str()));
            let artifacts = match (state.updates.as_ref(), digest) {
                (Some(updates), Some(digest)) => Some(updates.artifact_store_for_digest(digest).map_err(|_| error("MODULE_UNAVAILABLE", "proxy.artifact_authority", &id))?),
                _ => None,
            };
            let policy = if privilege.is_some() { ConfirmationPolicy::Required } else { ConfirmationPolicy::None };
            let registered = register(runtime, request, params.clone(), policy, &id)?;
            if registered.created {
                preparing(&runtime.jobs, &registered.job.job_id, &id)?;
                let runtime_worker = Arc::clone(runtime);
                let job_id = registered.job.job_id.clone();
                let method = method.to_owned();
                let updates = state.updates.clone();
                let handle = tokio::spawn(async move {
                    let _admission = admission;
                    let mut proxy = runtime_worker.proxy.lock().await;
                    if let Some(artifacts) = artifacts { proxy.set_artifact_store(artifacts); }
                    if let Some(input) = privilege {
                        if let Some(updates) = &updates {
                            let existing_cleanup = if cleanup(&input.operation) {
                                match updates.leases() {
                                    Ok(leases) => leases.iter().any(|lease|
                                        (lease.owner_id.starts_with("proxy-native-service:") || lease.owner_id.starts_with("proxy-native-privilege:"))
                                            && lease.digests.contains(&input.digest)),
                                    Err(_) => { runtime_worker.fail(&job_id, error("INTERNAL", "proxy.lease_read", &job_id)); return; }
                                }
                            } else { false };
                            match updates.pin_artifacts(&format!("proxy-native-privilege:{job_id}"), mintcat_runtime_updates::LeaseKind::Task, &[input.digest.clone()], None, true) {
                                Ok(_) => {},
                                Err(_) if existing_cleanup => {},
                                Err(_) => { runtime_worker.fail(&job_id, error("ARTIFACT_REVOKED", "proxy.privilege_artifact_admission", &job_id)); return; }
                            }
                        }
                        let expires = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                            Ok(time) => (time.as_millis().min(i64::MAX as u128) as i64).saturating_add(600_000),
                            Err(_) => { runtime_worker.fail(&job_id, error("INTERNAL", "proxy.clock", &job_id)); return; }
                        };
                        if let Err(failure) = proxy.request_privilege(&job_id, &input.digest, input.operation, expires) {
                            runtime_worker.fail(&job_id, proxy_error(&failure, "SYSTEM_AUTHORIZATION_REQUIRED", &job_id));
                        }
                        return;
                    }
                    if let Err(failure) = running(&runtime_worker.jobs, &job_id, &job_id) { runtime_worker.fail(&job_id, failure); return; }
                    let mut new_lease = None;
                    if let (Some(input), Some(updates)) = (&start, &updates) {
                        match proxy.status() {
                            Ok(status) if !status.running => match updates.pin_artifacts(&format!("proxy-native-service:{job_id}"), mintcat_runtime_updates::LeaseKind::Service, &[input.digest.clone()], None, true) {
                                Ok(lease) => new_lease = Some((updates.clone(), lease.lease_id)),
                                Err(_) => { runtime_worker.fail(&job_id, error("ARTIFACT_REVOKED", "proxy.artifact_admission", &job_id)); return; }
                            },
                            Ok(_) => {},
                            Err(failure) => { runtime_worker.fail(&job_id, proxy_error(&failure, "SERVICE_UNHEALTHY", &job_id)); return; }
                        }
                    }
                    let outcome = if let Some(input) = start { proxy.start(&input.digest, input.options).await } else { proxy.stop().await };
                    match outcome {
                        Ok(status) => {
                            if let Some(lease) = new_lease {
                                match runtime_worker.service_lease.lock() {
                                    Ok(mut slot) => *slot = Some(lease),
                                    Err(_) => { runtime_worker.fail(&job_id, error("INTERNAL", "proxy.lease", &job_id)); return; }
                                }
                            }
                            if !status.running {
                                if let Err(failure) = runtime_worker.release_service_lease() { runtime_worker.fail(&job_id, failure); return; }
                            }
                            match serde_json::to_value(status) {
                                Ok(result) => runtime_worker.succeeded(&job_id, result),
                                Err(_) => runtime_worker.fail(&job_id, error("INTERNAL", "proxy.result", &job_id)),
                            }
                        }
                        Err(failure) => {
                            if let Some((updates, lease_id)) = new_lease { if let Err(error) = updates.release_lease(&lease_id) { eprintln!("proxy failed-start lease release: {error}"); } }
                            runtime_worker.fail(&job_id, proxy_error(&failure, if method == "proxy.start" { "SERVICE_START_FAILED" } else { "SERVICE_STOP_FAILED" }, &job_id));
                        }
                    }
                });
                runtime.track(registered.job.job_id.clone(), Worker { handle, agent_session: None, privilege_execution: false })?;
            }
            encode(registered.job, &id)
        }
    }
}
fn owned_command(command: &str) -> bool { matches!(command, "agent.sessions.send" | "proxy.start" | "proxy.stop" | "proxy.privilege") }
fn cleanup(operation: &PrivilegeOperation) -> bool { matches!(operation, PrivilegeOperation::CleanupHosts | PrivilegeOperation::RemoveCertificate) }
fn admission(state: &DaemonState, module_id: &str, cleanup_only: bool, id: &str) -> Result<MaintenanceAdmission> {
    if module_id == MODULE_ID && !state.features.proxy_writes.load(std::sync::atomic::Ordering::Acquire) {
        return Err(error("PERMISSION_DENIED", "proxy.owner_read_only", id));
    }
    if !cleanup_only {
        if let Some(updates) = &state.updates {
            let environment = updates.environment().map_err(|_| error("MODULE_UNAVAILABLE", "features.scope", id))?;
            if let Some((scope, generation)) = environment.scopes.iter().find(|(_, generation)| generation.targets.signed.packages.values().any(|record| record.manifest.id == module_id)) {
                return updates.admit_mutation(scope, &generation.generation_id).map_err(|_| error("PERMISSION_DENIED", "features.scope_write_blocked", id));
            }
            return Err(error("MODULE_UNAVAILABLE", "features.confirmed_scope_missing", id));
        }
    }
    MaintenanceGate::enter_mutation(&state.layout).map_err(|_| error("PERMISSION_DENIED", "features.maintenance", id))
}
fn register(runtime: &FeatureRuntime, request: &ValidatedRequest, target: Value, policy: ConfirmationPolicy, id: &str) -> Result<mintcat_runtime_jobs::JobRegistration> {
    runtime.jobs.register_job(RegisterJob {
        command: request.envelope.method.clone(), owner_scope: LOCAL_OWNER_SCOPE.into(),
        idempotency_key: request.envelope.idempotency_key.clone().ok_or_else(|| error("INVALID_ARGUMENT", "features.idempotency_required", id))?,
        request_hash: request_hash(&request.envelope.params).map_err(|e| job_error(e, id))?, target, confirmation_policy: policy,
    }).map_err(|e| job_error(e, id))
}
fn preparing(jobs: &JobStore, job_id: &str, id: &str) -> Result<()> { jobs.append_event(job_id, EventUpdate::state("feature-preparing", JobState::Preparing)).map_err(|e| job_error(e, id))?; Ok(()) }
fn running(jobs: &JobStore, job_id: &str, id: &str) -> Result<()> {
    if jobs.cancel_at_safe_point(job_id).map_err(|e| job_error(e, id))?.state == JobState::Cancelled { return Err(error("CANCELLED", "features.cancelled", id)); }
    jobs.append_event(job_id, EventUpdate::state("feature-running", JobState::Running)).map_err(|e| job_error(e, id))?; Ok(())
}
async fn agent_call<T: Send + 'static>(runtime: &FeatureRuntime, call: impl FnOnce(Arc<NativeAgentSessions>) -> std::result::Result<T, AgentError> + Send + 'static, id: &str) -> Result<T> {
    let agent = Arc::clone(&runtime.agent);
    tokio::task::spawn_blocking(move || call(agent)).await.map_err(|_| error("INTERNAL", "agent.executor", id))?.map_err(|failure| agent_error(failure, id))
}
fn decode<T: serde::de::DeserializeOwned>(params: &Value, id: &str) -> Result<T> { serde_json::from_value(params.clone()).map_err(|_| error("INVALID_ARGUMENT", "features.invalid_argument", id)) }
fn encode<T: serde::Serialize>(value: T, id: &str) -> Result<Value> { serde_json::to_value(value).map_err(|_| error("INTERNAL", "features.result", id)) }
fn job_error(failure: mintcat_runtime_jobs::JobError, id: &str) -> Box<AppError> { Box::new(super::job_error(id, failure)) }
fn agent_error(failure: AgentError, id: &str) -> Box<AppError> { error(failure.code(), "agent.command_failed", id) }
fn proxy_error(failure: &anyhow::Error, fallback: &str, id: &str) -> Box<AppError> {
    if let Some(failure) = failure.downcast_ref::<mintcat_runtime_jobs::JobError>() {
        let code = match failure {
            mintcat_runtime_jobs::JobError::StalePlan | mintcat_runtime_jobs::JobError::ActionExpired => "STALE_PLAN",
            mintcat_runtime_jobs::JobError::ActionState(_) => "APPROVAL_REQUIRED",
            mintcat_runtime_jobs::JobError::PermissionDenied => "PERMISSION_DENIED",
            _ => fallback,
        };
        return error(code, "proxy.command_failed", id);
    }
    let message = failure.to_string();
    let code = if message.contains("revoked") { "ARTIFACT_REVOKED" }
        else if message.contains("protocol") || message.contains("identity") { "SERVICE_PROTOCOL_MISMATCH" }
        else if message.contains("approval") || message.contains("authorization") || message.contains("privileged") { "PERMISSION_DENIED" }
        else { fallback };
    error(code, "proxy.command_failed", id)
}
fn error(code: &str, key: &str, id: &str) -> Box<AppError> { Box::new(AppError { code: code.into(), message_key: key.into(), details: json!({}), request_id: id.into(), job_id: None, retryable: false, recovery_actions: Vec::new() }) }

struct ProxyCode {
    artifacts: ArtifactStore,
    digest: String,
    _lease: mintcat_runtime_artifacts::ModuleLease,
}
struct ProxyStateOwner {
    runtime: Weak<FeatureRuntime>,
    selected: Mutex<Option<ProxyCode>>,
    backups: std::path::PathBuf,
}
impl ProxyStateOwner {
    fn runtime(&self) -> mintcat_runtime_updates::Result<Arc<FeatureRuntime>> {
        self.runtime.upgrade().ok_or_else(|| mintcat_runtime_updates::UpdateError::Blocked("Proxy daemon owner unavailable".into()))
    }
    async fn select(&self, generation: &mintcat_runtime_updates::ScopeGeneration, artifacts: ArtifactStore) -> mintcat_runtime_updates::Result<()> {
        let mut packages = generation.targets.signed.packages.iter().filter(|(_, record)| record.manifest.id == MODULE_ID);
        let (digest, record) = packages.next().ok_or_else(|| mintcat_runtime_updates::UpdateError::Blocked("generation lacks signed Proxy service".into()))?;
        proxy_require(packages.next().is_none(), "generation has multiple Proxy services")?;
        <Self as mintcat_runtime_updates::StateOwner>::supports(self, None, &record.manifest)?;
        proxy_require(record.digest == *digest, "Proxy package digest differs from signed generation")?;
        let installed = artifacts.load_module(digest)?;
        proxy_require(installed.manifest == record.manifest && installed.signature == record.signature, "Proxy installed package differs from exact signed generation")?;
        let root = mintcat_runtime_release::trust_pinned_root(generation.verified_root.clone(), 0)?;
        root.verify("targets", &generation.targets)?;
        root.verify_package_signature(&installed.manifest, &installed.signature, &mintcat_runtime_artifacts::module_manifest_signing_digest(&installed.manifest)?)?;
        let lease = verify_proxy_code(&artifacts, digest).await?;
        proxy_require(lease.module().manifest == record.manifest && lease.module().signature == record.signature, "installed Proxy code differs from signed generation")?;
        *self.selected.lock().map_err(|_| mintcat_runtime_updates::UpdateError::Blocked("Proxy selection lock poisoned".into()))? = Some(ProxyCode { artifacts, digest: digest.clone(), _lease: lease });
        Ok(())
    }
    fn fingerprint(runtime: &FeatureRuntime) -> mintcat_runtime_updates::Result<Value> {
        let mut state = serde_json::Map::new();
        for name in ["ca.pem", "ca.key", "accelerate_cache.json", "custom_rules.json", "hosts-owner.json", "revoked-artifacts.json", "write-epoch.json"] {
            let path = runtime.proxy_data.join(name);
            if path.try_exists()? {
                let metadata = std::fs::symlink_metadata(&path)?;
                proxy_require(metadata.is_file() && !metadata.file_type().is_symlink(), "Proxy owner state is not a regular owned file")?;
                use sha2::{Digest, Sha256};
                let mut input = std::fs::File::open(path)?;
                let mut digest = Sha256::new();
                std::io::copy(&mut input, &mut digest)?;
                state.insert(name.into(), Value::String(format!("sha256:{:x}", digest.finalize())));
            } else { state.insert(name.into(), Value::Null); }
        }
        Ok(Value::Object(state))
    }
}
impl mintcat_runtime_updates::StateOwner for ProxyStateOwner {
    fn owner_id(&self) -> &str { MODULE_ID }
    fn supports(&self, old: Option<&mintcat_sdk::ModuleManifest>, new: &mintcat_sdk::ModuleManifest) -> mintcat_runtime_updates::Result<()> {
        proxy_require(new.id == MODULE_ID && new.state.schema_version == 1 && old.is_none_or(|manifest| manifest.id == MODULE_ID && manifest.state.schema_version == 1),
            "Proxy state migration unsupported; only preserved schema1 native code updates are supported")?;
        proxy_require(new.components.iter().filter(|component| self.handles_native(new, component)).count() == 1
            && new.components.iter().all(|component| component.state_model == "stateless" || self.handles_native(new, component)),
            "Proxy package requires exactly one signed native protocol service and no unhandled state owner")
    }
    fn handles_native(&self, manifest: &mintcat_sdk::ModuleManifest, component: &mintcat_sdk::ModuleComponent) -> bool {
        manifest.id == MODULE_ID && manifest.state.schema_version == 1 && component.kind == "service"
            && component.runtime == mintcat_feature_proxy::SERVICE_PROTOCOL && component.state_model == "daemon-owned"
            && manifest.files.iter().any(|file| file.path == component.entry && file.executable)
    }
    fn prepare_code<'a>(&'a self, generation: &'a mintcat_runtime_updates::ScopeGeneration, artifacts: ArtifactStore) -> mintcat_runtime_updates::OwnerFuture<'a, ()> {
        Box::pin(async move { self.select(generation, artifacts).await })
    }
    fn rollback_code<'a>(&'a self, generation: &'a mintcat_runtime_updates::ScopeGeneration, artifacts: ArtifactStore) -> mintcat_runtime_updates::OwnerFuture<'a, ()> {
        // Same-schema rollback preserves current CA/rules/hosts and the old running controller.
        Box::pin(async move { self.select(generation, artifacts).await })
    }
    fn deactivate_code<'a>(&'a self, generation: &'a mintcat_runtime_updates::ScopeGeneration) -> mintcat_runtime_updates::OwnerFuture<'a, ()> {
        Box::pin(async move {
            let digest = generation.targets.signed.packages.iter().find(|(_, package)| package.manifest.id == MODULE_ID)
                .map(|(digest, _)| digest).ok_or_else(|| mintcat_runtime_updates::UpdateError::Blocked("Proxy deactivation generation has no signed owner".into()))?;
            let runtime = self.runtime()?;
            let _proxy = runtime.proxy.try_lock().map_err(|_| mintcat_runtime_updates::UpdateError::Busy("Proxy mutation must reach its safe point".into()))?;
            let mut selected = self.selected.lock().map_err(|_| mintcat_runtime_updates::UpdateError::Blocked("Proxy selection lock poisoned".into()))?;
            proxy_require(selected.as_ref().is_none_or(|code| code.digest == *digest), "Proxy deactivation would deselect another signed generation")?;
            runtime.proxy_writes.store(false, std::sync::atomic::Ordering::Release);
            // Existing service controllers keep their startup lease and cleanup path.
            // Only new admissions lose this code selection; CA/rules/hosts are untouched.
            *selected = None;
            Ok(())
        })
    }
    fn drain(&self) -> mintcat_runtime_updates::OwnerFuture<'_, ()> {
        Box::pin(async move {
            let runtime = self.runtime()?;
            let _proxy = runtime.proxy.try_lock().map_err(|_| mintcat_runtime_updates::UpdateError::Busy("Proxy mutation must reach its safe point".into()))?;
            runtime.proxy_writes.store(false, std::sync::atomic::Ordering::Release);
            Ok(())
        })
    }
    fn backup<'a>(&'a self, old_schema: u64, new_schema: u64) -> mintcat_runtime_updates::OwnerFuture<'a, mintcat_runtime_updates::OwnerBackup> {
        Box::pin(async move {
            proxy_require(old_schema == 1 && new_schema == 1, "Proxy schema migration unsupported")?;
            let runtime = self.runtime()?;
            proxy_require(!runtime.proxy_writes.load(std::sync::atomic::Ordering::Acquire), "Proxy backup requires drained writes")?;
            let proxy = runtime.proxy.lock().await;
            let receipt = json!({"formatVersion":1,"writeEpoch":proxy.write_epoch(),"fingerprint":Self::fingerprint(&runtime)?});
            std::fs::create_dir_all(&self.backups)?;
            #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&self.backups, std::fs::Permissions::from_mode(0o700))?; }
            let name = format!("{}.json", uuid::Uuid::new_v4());
            let bytes = serde_json::to_vec(&receipt)?;
            let mut options = std::fs::OpenOptions::new(); options.write(true).create_new(true);
            #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
            let mut file = options.open(self.backups.join(&name))?;
            use std::io::Write;
            file.write_all(&bytes)?; file.sync_all()?;
            std::fs::File::open(&self.backups)?.sync_all()?;
            Ok(mintcat_runtime_updates::OwnerBackup { owner_id: MODULE_ID.into(), old_schema, new_schema,
                reference: json!({"snapshotName":name,"sha256":mintcat_runtime_release::sha256(&bytes),"writeEpoch":proxy.write_epoch()}) })
        })
    }
    fn migrate<'a>(&'a self, _: &'a mintcat_runtime_updates::OwnerBackup) -> mintcat_runtime_updates::OwnerFuture<'a, ()> {
        Box::pin(async { Err(mintcat_runtime_updates::UpdateError::Blocked("Proxy schema migration unsupported; code activation preserves schema1 owner state".into())) })
    }
    fn read_only_health(&self) -> mintcat_runtime_updates::OwnerFuture<'_, ()> {
        Box::pin(async move {
            let (artifacts, digest) = self.selected.lock().map_err(|_| mintcat_runtime_updates::UpdateError::Blocked("Proxy selection lock poisoned".into()))?
                .as_ref().map(|code| (code.artifacts.clone(), code.digest.clone()))
                .ok_or_else(|| mintcat_runtime_updates::UpdateError::Blocked("Proxy signed native code not prepared".into()))?;
            verify_proxy_code(&artifacts, &digest).await?;
            Self::fingerprint(self.runtime()?.as_ref())?;
            Ok(())
        })
    }
    fn restore<'a>(&'a self, backup: &'a mintcat_runtime_updates::OwnerBackup) -> mintcat_runtime_updates::OwnerFuture<'a, ()> {
        Box::pin(async move {
            proxy_require(backup.owner_id == MODULE_ID && backup.old_schema == 1 && backup.new_schema == 1, "Proxy backup owner/schema mismatch")?;
            let name = backup.reference["snapshotName"].as_str().ok_or_else(|| mintcat_runtime_updates::UpdateError::Blocked("Proxy receipt name missing".into()))?;
            proxy_require(name.strip_suffix(".json").is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()), "Proxy receipt name invalid")?;
            let path = self.backups.join(name);
            let metadata = std::fs::symlink_metadata(&path)?;
            proxy_require(metadata.is_file() && !metadata.file_type().is_symlink(), "Proxy receipt must be regular metadata")?;
            let bytes = std::fs::read(path)?;
            proxy_require(backup.reference["sha256"].as_str() == Some(mintcat_runtime_release::sha256(&bytes).as_str()), "Proxy receipt digest mismatch")?;
            let receipt: Value = serde_json::from_slice(&bytes)?;
            let runtime = self.runtime()?;
            proxy_require(!runtime.proxy_writes.load(std::sync::atomic::Ordering::Acquire), "Proxy restore requires drained writes")?;
            let proxy = runtime.proxy.lock().await;
            proxy_require(receipt["writeEpoch"].as_u64() == Some(proxy.write_epoch()) && receipt["fingerprint"] == Self::fingerprint(&runtime)?,
                "Proxy owner changed after snapshot; restoring CA/hosts/rules or older metadata is prohibited")?;
            // Preservation is verified, never overwrite system credentials or hosts.
            Ok(())
        })
    }
    fn open_writes(&self) -> mintcat_runtime_updates::OwnerFuture<'_, ()> {
        Box::pin(async move {
            let artifacts = self.selected.lock().map_err(|_| mintcat_runtime_updates::UpdateError::Blocked("Proxy selection lock poisoned".into()))?
                .as_ref().map(|code| code.artifacts.clone()).ok_or_else(|| mintcat_runtime_updates::UpdateError::Blocked("Proxy native code not prepared".into()))?;
            let runtime = self.runtime()?;
            runtime.proxy.lock().await.set_artifact_store(artifacts);
            runtime.proxy_writes.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        })
    }
}
fn proxy_require(ok: bool, reason: &'static str) -> mintcat_runtime_updates::Result<()> {
    if ok { Ok(()) } else { Err(mintcat_runtime_updates::UpdateError::Blocked(reason.into())) }
}
async fn verify_proxy_code(artifacts: &ArtifactStore, digest: &str) -> mintcat_runtime_updates::Result<mintcat_runtime_artifacts::ModuleLease> {
    use tokio::io::AsyncReadExt;
    let lease = artifacts.acquire_module(digest)?;
    let manifest = &lease.module().manifest;
    proxy_require(manifest.id == MODULE_ID && manifest.state.schema_version == 1, "Proxy signed package/schema mismatch")?;
    let component = manifest.components.iter().find(|component| component.kind == "service" && component.runtime == mintcat_feature_proxy::SERVICE_PROTOCOL && component.state_model == "daemon-owned")
        .ok_or_else(|| mintcat_runtime_updates::UpdateError::Blocked("signed Proxy native protocol missing".into()))?;
    proxy_require(manifest.files.iter().any(|file| file.path == component.entry && file.executable), "Proxy native entry is not signed executable content")?;
    let identity = mintcat_feature_proxy::ServiceIdentity { protocol: mintcat_feature_proxy::SERVICE_PROTOCOL.into(), module_digest: digest.into(),
        module_version: manifest.version.clone(), instance_id: format!("proxy-probe:{}", uuid::Uuid::new_v4()) };
    let mut child = tokio::process::Command::new(lease.module().entry_path(&component.entry)?)
        .env_clear().env("MINTCAT_PROXY_DIGEST", digest).env("MINTCAT_PROXY_VERSION", &manifest.version).env("MINTCAT_PROXY_INSTANCE", &identity.instance_id)
        .arg("--verify-protocol").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn()?;
    let mut output = child.stdout.take().ok_or_else(|| mintcat_runtime_updates::UpdateError::Blocked("Proxy probe stdout missing".into()))?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut bytes = Vec::new(); (&mut output).take(4097).read_to_end(&mut bytes).await?;
        proxy_require(bytes.len() <= 4096 && bytes.last() == Some(&b'\n'), "Proxy native health frame invalid")?;
        let actual: mintcat_feature_proxy::ServiceIdentity = serde_json::from_slice(&bytes)?;
        proxy_require(actual == identity, "Proxy signed executable protocol/version/identity mismatch")?;
        proxy_require(child.wait().await?.success(), "Proxy native health exited unsuccessfully")?;
        Ok::<(), mintcat_runtime_updates::UpdateError>(())
    }).await;
    match result {
        Ok(Ok(())) => Ok(lease),
        result => {
            let _ = child.kill().await; let _ = child.wait().await;
            match result { Ok(Err(error)) => Err(error), _ => Err(mintcat_runtime_updates::UpdateError::Blocked("Proxy native health deadline expired".into())) }
        }
    }
}

impl mintcat_feature_backup::PortableOwners for FeatureRuntime {
    fn snapshot(&self)->std::result::Result<std::collections::BTreeMap<String,mintcat_feature_backup::OwnerSnapshot>,mintcat_feature_backup::BackupError> {
        use mintcat_feature_backup::{BackupError,OwnerSnapshot};
        let agent=self.agent.portable_snapshot().map_err(|e|BackupError::Invalid(e.to_string()))?;
        let mut owners = std::collections::BTreeMap::from([
            ("mintcat.feature.agent".into(),OwnerSnapshot {schema_version:1,credential_policy:"excluded-reauthenticate-no-live-authority".into(),
                excluded_fields:vec!["configuration.credentialRef".into(),"live-task-bindings-converted-to-non-authoritative-timeline".into(),"idempotency-receipts".into(),"provider-tool-authority-and-private-history".into()],
                content:serde_json::to_value(agent)?}),
        ]);
        let proxy=self.proxy.try_lock().map_err(|_|BackupError::Busy("Proxy owner held".into()))?;
        if self.proxy_portable_owner_available()? {
            let config=proxy.portable_snapshot().map_err(|error|BackupError::Busy(error.to_string()))?;
            owners.insert(MODULE_ID.into(),OwnerSnapshot {schema_version:1,credential_policy:"excluded-reauthenticate-no-live-authority".into(),
                excluded_fields:vec!["ca.pem".into(),"ca.key".into(),"hosts-owner.json".into(),"revocations-and-authorized-ports".into(),"running-controller-and-startup-service-lease".into()],
                content:serde_json::to_value(config)?});
        } else {
            proxy.ensure_empty_portable_target().map_err(|error|BackupError::Module(format!("Proxy signed owner unavailable; refusing to omit existing state: {error}")))?;
        }
        Ok(owners)
    }
    fn ensure_empty(&self)->std::result::Result<(),mintcat_feature_backup::BackupError> {
        use mintcat_feature_backup::BackupError;
        if self.agent.has_nonempty().map_err(|e|BackupError::Module(e.to_string()))? {return Err(BackupError::Busy("Agent owner is nonempty".into()));}
        self.proxy.try_lock().map_err(|_|BackupError::Busy("Proxy owner held".into()))?.ensure_empty_portable_target().map_err(|e|BackupError::Busy(e.to_string()))?;
        Ok(())
    }
    fn validate(&self,owners:&std::collections::BTreeMap<String,mintcat_feature_backup::OwnerSnapshot>)->std::result::Result<(),mintcat_feature_backup::BackupError> {
        use mintcat_feature_backup::BackupError;
        let sessions:Vec<mintcat_feature_agent::AgentSession>=serde_json::from_value(owners.get("mintcat.feature.agent").ok_or_else(||BackupError::Module("Agent snapshot absent".into()))?.content.clone())?;
        self.agent.validate_portable(&sessions).map_err(|e|BackupError::Invalid(e.to_string()))?;
        if self.proxy_portable_owner_available()? != owners.contains_key(MODULE_ID) {
            return Err(BackupError::Module("Proxy snapshot does not match the admitted signed owner set".into()));
        }
        if let Some(owner) = owners.get(MODULE_ID) {
            let config:std::collections::BTreeMap<String,Value>=serde_json::from_value(owner.content.clone())?;
            ProxyServiceManager::validate_portable(&config).map_err(|error|BackupError::Invalid(error.to_string()))?;
        }
        Ok(())
    }
    fn write_epochs(&self)->std::result::Result<std::collections::BTreeMap<String,u64>,mintcat_feature_backup::BackupError> {
        let value=self.portable_epochs().map_err(|e|mintcat_feature_backup::BackupError::Module(format!("{}: {}",e.code,e.message_key)))?;
        Ok(serde_json::from_value(value)?)
    }
    fn import_empty(&self,owners:&std::collections::BTreeMap<String,mintcat_feature_backup::OwnerSnapshot>)->std::result::Result<(),mintcat_feature_backup::BackupError> {
        use mintcat_feature_backup::BackupError;
        self.ensure_empty()?;
        let sessions:Vec<mintcat_feature_agent::AgentSession>=serde_json::from_value(owners.get("mintcat.feature.agent").ok_or_else(||BackupError::Module("Agent snapshot absent".into()))?.content.clone())?;
        let config = owners.get(MODULE_ID).map(|owner|serde_json::from_value::<std::collections::BTreeMap<String,Value>>(owner.content.clone())).transpose()?;
        // The daemon's exclusive admission spans both owner-local commits.
        let epoch=self.agent.write_epoch().map_err(|e|BackupError::Module(e.to_string()))?;
        self.agent.import_empty_portable(&sessions,epoch).map_err(|e|BackupError::Invalid(e.to_string()))?;
        if let Some(config) = config {
            let mut proxy=self.proxy.try_lock().map_err(|_|BackupError::Busy("Proxy owner held".into()))?;
            let epoch=proxy.write_epoch();
            proxy.import_empty_portable(&config,epoch).map_err(|error|BackupError::Invalid(error.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod canonical_failure_tests {
    use super::*;
    #[test]
    fn failed_agent_job_remains_queryable_through_canonical_result_schema() {
        let sandbox=tempfile::tempdir().unwrap();
        let layout=mintcat_runtime_kernel::RuntimePaths::new(sandbox.path()).unwrap().initialize(mintcat_runtime_kernel::RuntimeEnvironment::Test).unwrap();
        let jobs=JobStore::open(mintcat_runtime_kernel::KernelDatabase::open(&layout).unwrap(),"failure-schema",32).unwrap();
        let job=jobs.register_job(RegisterJob {command:"agent.sessions.send".into(),owner_scope:LOCAL_OWNER_SCOPE.into(),idempotency_key:"provider-failure".into(),request_hash:"provider-failure".into(),target:json!({}),confirmation_policy:ConfirmationPolicy::None}).unwrap().job;
        let mut update=EventUpdate::state("agent-failed",JobState::Failed);
        update.error=Some(*agent_error(AgentError::Provider,&job.job_id));
        jobs.append_event(&job.job_id,update).unwrap();
        let result=serde_json::to_value(jobs.get_job(LOCAL_OWNER_SCOPE,&job.job_id).unwrap()).unwrap();
        assert_eq!(result["state"],"failed");
        assert_eq!(result["error"]["code"],"AGENT_PROVIDER");
        let version=mintcat_sdk::command_spec("jobs.get").unwrap().contract_version;
        mintcat_sdk::validate_command_result("jobs.get",version,&result).unwrap();
    }
}
