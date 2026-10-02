use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use mintcat_runtime_artifacts::{ArtifactError, ArtifactGrant, ArtifactStore, ModuleLease};
use mintcat_sdk::{
    read_worker_message, write_worker_message, AppError, ScopedProviderBinding, WorkerHello,
    WorkerHostHello, WorkerProtocolError, WorkerRequest, WorkerRequestKind, WorkerResponse,
    WorkerOutbound, WorkerBrokerRequest, ArtifactGrantDescriptor,
    WORKER_INSTANCE_ID_ENV, WORKER_MODULE_DIGEST_ENV, WORKER_MODULE_ID_ENV,
    WORKER_MODULE_VERSION_ENV, WORKER_PROTOCOL_VERSION, WORKER_PROVIDES_ENV, WORKER_WORKSPACE_ENV,
};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::{
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex as AsyncMutex,
    time,
};
use uuid::Uuid;

pub const WORKER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
pub const WORKER_HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
pub const WORKER_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

const STATE_ACTIVE: u8 = 0;
const STATE_DRAINING: u8 = 1;
const STATE_STOPPED: u8 = 2;
const STATE_CRASHED: u8 = 3;

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error("module supervisor I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("worker protocol failed: {0}")]
    Protocol(#[from] WorkerProtocolError),
    #[error("worker handshake exceeded its deadline")]
    HandshakeTimeout,
    #[error("worker request exceeded its deadline")]
    RequestTimeout,
    #[error("worker drain exceeded its deadline")]
    DrainTimeout,
    #[error("provider binding is invalid: {0}")]
    InvalidBinding(String),
    #[error("required contract {contract_id}@{version_range} has no binding in scope {scope_id}")]
    UnsatisfiedRequirement {
        scope_id: String,
        contract_id: String,
        version_range: String,
    },
    #[error("stateful module {0} already has an active version")]
    StatefulVersionConflict(String),
    #[error("provider {contract_id}@{contract_version} is not bound in scope {scope_id}")]
    ProviderUnavailable {
        scope_id: String,
        contract_id: String,
        contract_version: u64,
    },
    #[error("worker instance {0} is draining or stopped")]
    InstanceUnavailable(String),
    #[error("worker {instance_id} handshake does not match its verified manifest: {message}")]
    HandshakeMismatch {
        instance_id: String,
        message: String,
    },
    #[error("worker response id {actual} does not match request id {expected}")]
    ResponseMismatch { expected: String, actual: String },
    #[error("worker returned {code}: {message_key}")]
    WorkerApplication { code: String, message_key: String },
    #[error("worker instance {instance_id} exited unexpectedly{status}")]
    WorkerExited { instance_id: String, status: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceLifecycle {
    Active,
    Draining,
    Stopped,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSnapshot {
    pub instance_id: String,
    pub module_id: String,
    pub module_version: String,
    pub module_digest: String,
    pub lifecycle: InstanceLifecycle,
    pub active_requests: usize,
    pub log_path: PathBuf,
}

#[derive(Debug)]
pub struct Invocation<'a> {
    pub scope_id: &'a str,
    pub contract_id: &'a str,
    pub contract_version: u64,
    pub method: &'a str,
    pub params: Value,
    pub grants: &'a [ArtifactGrant],
    pub deadline: Duration,
}

#[derive(Debug, Clone)]
pub struct HostServiceCaller {
    pub module_id: String,
    pub module_digest: String,
}

#[async_trait::async_trait]
pub trait HostService: Send + Sync {
    async fn execute(
        &self,
        caller: HostServiceCaller,
        method: &str,
        params: Value,
    ) -> Result<Value, SupervisorError>;
}

type HostServices = HashMap<(String, u64), Arc<dyn HostService>>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProviderKey {
    scope_id: String,
    contract_id: String,
    contract_version: u64,
}

impl From<&ScopedProviderBinding> for ProviderKey {
    fn from(binding: &ScopedProviderBinding) -> Self {
        Self {
            scope_id: binding.scope_id.clone(),
            contract_id: binding.contract_id.clone(),
            contract_version: binding.contract_version,
        }
    }
}

#[derive(Default)]
struct RegistryState {
    bindings: HashMap<ProviderKey, String>,
    instances: HashMap<String, Arc<WorkerInstance>>,
}

struct SupervisorInner {
    artifacts: ArtifactStore,
    generation_id: String,
    logs_dir: PathBuf,
    registry: Mutex<RegistryState>,
    mutations: AsyncMutex<()>,
    host_services: Mutex<HostServices>,
}

#[derive(Clone)]
pub struct ModuleSupervisor {
    inner: Arc<SupervisorInner>,
}

struct WorkerInstance {
    instance_id: String,
    module_id: String,
    module_version: String,
    module_digest: String,
    log_path: PathBuf,
    workspace: PathBuf,
    lifecycle: AtomicU8,
    active_requests: AtomicUsize,
    connection: AsyncMutex<WorkerConnection>,
    _module_lease: ModuleLease,
}

struct WorkerConnection {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
}

struct BrokerContext<'a> {
    artifacts: &'a ArtifactStore,
    producer_workspace: &'a Path,
    engine: Option<Arc<WorkerInstance>>,
    original: &'a [ArtifactGrant],
    caller: &'a WorkerInstance,
    host_services: &'a Mutex<HostServices>,
}

struct ActiveRequest<'a> {
    count: &'a AtomicUsize,
}

impl Drop for ActiveRequest<'_> {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ModuleSupervisor {
    pub fn new(
        artifacts: ArtifactStore,
        generation_id: impl Into<String>,
        runtime_dir: &Path,
    ) -> Result<Self, SupervisorError> {
        let logs_dir = runtime_dir.join("module-logs");
        fs::create_dir_all(&logs_dir)?;
        Ok(Self {
            inner: Arc::new(SupervisorInner {
                artifacts,
                generation_id: generation_id.into(),
                logs_dir,
                registry: Mutex::new(RegistryState::default()),
                mutations: AsyncMutex::new(()),
                host_services: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn artifacts(&self) -> &ArtifactStore {
        &self.inner.artifacts
    }

    pub fn grant_output(
        &self,
        scope_id: &str,
        contract_id: &str,
        contract_version: u64,
        descriptor: &ArtifactGrantDescriptor,
    ) -> Result<ArtifactGrant, SupervisorError> {
        let registry = self.inner.registry.lock().expect("module registry poisoned");
        let key = ProviderKey {
            scope_id: scope_id.to_owned(), contract_id: contract_id.to_owned(), contract_version,
        };
        let instance_id = registry.bindings.get(&key).ok_or_else(|| SupervisorError::ProviderUnavailable {
            scope_id: scope_id.to_owned(), contract_id: contract_id.to_owned(), contract_version,
        })?;
        let instance = registry.instances.get(instance_id).expect("binding instance exists");
        Ok(self.inner.artifacts.grant_produced(&instance.workspace, descriptor)?)
    }

    pub fn register_host_service(
        &self,
        contract_id: &str,
        contract_version: u64,
        service: Arc<dyn HostService>,
    ) -> Result<(), SupervisorError> {
        if contract_id != "source.network" || contract_version != 1 {
            return Err(SupervisorError::InvalidBinding("unapproved host service contract".into()));
        }
        let mut services = self.inner.host_services.lock().expect("host services poisoned");
        let key = (contract_id.to_owned(), contract_version);
        if services.contains_key(&key) {
            return Err(SupervisorError::InvalidBinding("host service already registered".into()));
        }
        services.insert(key, service);
        Ok(())
    }

    pub async fn activate(
        &self,
        binding: ScopedProviderBinding,
    ) -> Result<String, SupervisorError> {
        let _mutation = self.inner.mutations.lock().await;
        let key = ProviderKey::from(&binding);
        let existing = self.inner.registry.lock().expect("module registry poisoned")
            .bindings.get(&key).cloned();
        if let Some(existing) = existing {
            let registry = self.inner.registry.lock().expect("module registry poisoned");
            let instance = registry.instances.get(&existing).expect("binding instance exists");
            if instance.module_digest != binding.provider_digest {
                return Err(SupervisorError::InvalidBinding("immutable scope binding cannot change provider digest".into()));
            }
            return Ok(existing);
        }

        let lease = self
            .inner
            .artifacts
            .acquire_module(&binding.provider_digest)?;
        let module = lease.module();
        if module.manifest.id != binding.provider_module_id {
            return Err(SupervisorError::InvalidBinding(format!(
                "binding names module {}, artifact contains {}",
                binding.provider_module_id, module.manifest.id
            )));
        }
        let provides_contract = module.manifest.provides.iter().any(|provided| {
            provided.contract_id == binding.contract_id
                && provided.version == binding.contract_version
        });
        if !provides_contract {
            return Err(SupervisorError::InvalidBinding(format!(
                "module {} does not provide {}@{}",
                module.manifest.id, binding.contract_id, binding.contract_version
            )));
        }
        let worker_components: Vec<_> = module
            .manifest
            .components
            .iter()
            .filter(|component| component.kind == "service" && component.runtime == "worker")
            .collect();
        if worker_components.len() != 1 {
            return Err(SupervisorError::InvalidBinding(format!(
                "module {} must contain exactly one service worker component",
                module.manifest.id
            )));
        }
        let component = worker_components[0];
        let component_entry = component.entry.clone();
        let component_is_stateless = component.state_model == "stateless";
        self.validate_requirements(&binding.scope_id, &module.manifest.requires)?;

        {
            let mut registry = self
                .inner
                .registry
                .lock()
                .expect("module registry poisoned");
            if let Some(existing) = registry
                .instances
                .values()
                .find(|instance| {
                    instance.module_digest == module.digest
                        && instance.lifecycle() == InstanceLifecycle::Active
                })
                .cloned()
            {
                registry.bindings.insert(key, existing.instance_id.clone());
                return Ok(existing.instance_id.clone());
            }
            if !component_is_stateless
                && registry.instances.values().any(|instance| {
                    instance.module_id == module.manifest.id
                        && matches!(
                            instance.lifecycle(),
                            InstanceLifecycle::Active | InstanceLifecycle::Draining
                        )
                })
            {
                return Err(SupervisorError::StatefulVersionConflict(
                    module.manifest.id.clone(),
                ));
            }
        }

        let instance = Arc::new(self.spawn_worker(lease, &component_entry).await?);
        let instance_id = instance.instance_id.clone();
        let mut registry = self
            .inner
            .registry
            .lock()
            .expect("module registry poisoned");
        registry.bindings.insert(key, instance_id.clone());
        registry.instances.insert(instance_id.clone(), instance);
        Ok(instance_id)
    }

    pub async fn invoke(&self, invocation: Invocation<'_>) -> Result<Value, SupervisorError> {
        let Invocation {
            scope_id,
            contract_id,
            contract_version,
            method,
            params,
            grants,
            deadline,
        } = invocation;
        let key = ProviderKey {
            scope_id: scope_id.to_owned(),
            contract_id: contract_id.to_owned(),
            contract_version,
        };
        let (instance, engine) = {
            let registry = self
                .inner
                .registry
                .lock()
                .expect("module registry poisoned");
            let instance_id = registry.bindings.get(&key).ok_or_else(|| {
                SupervisorError::ProviderUnavailable {
                    scope_id: scope_id.to_owned(),
                    contract_id: contract_id.to_owned(),
                    contract_version,
                }
            })?;
            let instance = registry.instances.get(instance_id)
                .expect("binding references a registered instance").clone();
            let requires_engine = instance._module_lease.module().manifest.requires.iter().any(|required| {
                required.contract_id == "engine.unreal-pak" && version_matches(&required.version_range, 1)
            });
            let engine = if requires_engine {
                let engine_key = ProviderKey { scope_id: scope_id.to_owned(), contract_id: "engine.unreal-pak".into(), contract_version: 1 };
                let engine_id = registry.bindings.get(&engine_key).ok_or_else(|| SupervisorError::ProviderUnavailable {
                    scope_id: scope_id.to_owned(), contract_id: "engine.unreal-pak".into(), contract_version: 1,
                })?;
                let engine = registry.instances.get(engine_id).expect("binding references a registered instance").clone();
                if engine.instance_id == instance.instance_id || !engine._module_lease.module().manifest.requires.is_empty() {
                    return Err(SupervisorError::InvalidBinding("engine callbacks require a distinct leaf provider".into()));
                }
                Some(engine)
            } else { None };
            (instance, engine)
        };
        instance
            .invoke(
                contract_id,
                contract_version,
                method,
                params,
                grants,
                deadline,
                Some(&BrokerContext {
                    artifacts: &self.inner.artifacts,
                    producer_workspace: &instance.workspace,
                    engine,
                    original: grants,
                    caller: &instance,
                    host_services: &self.inner.host_services,
                }),
            )
            .await
    }

    pub async fn drain_binding(
        &self,
        scope_id: &str,
        contract_id: &str,
        contract_version: u64,
    ) -> Result<(), SupervisorError> {
        let _mutation = self.inner.mutations.lock().await;
        let key = ProviderKey {
            scope_id: scope_id.to_owned(),
            contract_id: contract_id.to_owned(),
            contract_version,
        };
        let instance = {
            let mut registry = self
                .inner
                .registry
                .lock()
                .expect("module registry poisoned");
            let instance_id = registry.bindings.remove(&key).ok_or_else(|| {
                SupervisorError::ProviderUnavailable {
                    scope_id: scope_id.to_owned(),
                    contract_id: contract_id.to_owned(),
                    contract_version,
                }
            })?;
            if registry
                .bindings
                .values()
                .any(|candidate| candidate == &instance_id)
            {
                return Ok(());
            }
            registry
                .instances
                .get(&instance_id)
                .expect("binding references a registered instance")
                .clone()
        };
        let result = instance.drain().await;
        self.inner
            .registry
            .lock()
            .expect("module registry poisoned")
            .instances
            .remove(&instance.instance_id);
        result
    }

    pub async fn shutdown_all(&self) -> Result<(), SupervisorError> {
        let _mutation = self.inner.mutations.lock().await;
        let instances = {
            let mut registry = self
                .inner
                .registry
                .lock()
                .expect("module registry poisoned");
            registry.bindings.clear();
            registry
                .instances
                .drain()
                .map(|(_, instance)| instance)
                .collect::<Vec<_>>()
        };
        let mut first_error = None;
        for instance in instances {
            if let Err(error) = instance.drain().await {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn snapshots(&self) -> Vec<InstanceSnapshot> {
        let registry = self
            .inner
            .registry
            .lock()
            .expect("module registry poisoned");
        let mut snapshots = registry
            .instances
            .values()
            .map(|instance| instance.snapshot())
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
        snapshots
    }

    /// Runs the real worker read-only health exchange; cached lifecycle is not readiness.
    pub async fn health_check(&self) -> Result<(), SupervisorError> {
        let instances = self.inner.registry.lock().expect("module registry poisoned")
            .instances.values().cloned().collect::<Vec<_>>();
        for instance in instances {
            if instance.lifecycle() != InstanceLifecycle::Active {
                return Err(SupervisorError::InstanceUnavailable(instance.instance_id.clone()));
            }
            let request = WorkerRequest {
                id: Uuid::new_v4().to_string(), kind: WorkerRequestKind::Health,
                contract_id: None, contract_version: None, method: None,
                params: json!({}), grants: Vec::new(),
            };
            let mut connection = instance.connection.lock().await;
            let result = call_worker(&instance.instance_id, &mut connection, &request, WORKER_HEALTH_TIMEOUT).await;
            match result {
                Ok(value) if value.get("status").and_then(Value::as_str) == Some("healthy") => {}
                Ok(_) => {
                    instance.lifecycle.store(STATE_CRASHED, Ordering::Release);
                    terminate(&mut connection.child).await;
                    return Err(SupervisorError::HandshakeMismatch { instance_id: instance.instance_id.clone(), message: "read-only health response was not healthy".into() });
                }
                Err(error) => {
                    instance.lifecycle.store(STATE_CRASHED, Ordering::Release);
                    terminate(&mut connection.child).await;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn validate_requirements(
        &self,
        scope_id: &str,
        requirements: &[mintcat_sdk::RequiredContract],
    ) -> Result<(), SupervisorError> {
        let registry = self
            .inner
            .registry
            .lock()
            .expect("module registry poisoned");
        let host_services = self.inner.host_services.lock().expect("host services poisoned");
        for requirement in requirements
            .iter()
            .filter(|requirement| !requirement.optional)
        {
            let satisfied = if requirement.binding_scope.as_deref() == Some("host") {
                host_services.keys().any(|(contract, version)| {
                    contract == &requirement.contract_id
                        && version_matches(&requirement.version_range, *version)
                })
            } else {
                registry.bindings.keys().any(|key| {
                    key.scope_id == scope_id
                        && key.contract_id == requirement.contract_id
                        && version_matches(&requirement.version_range, key.contract_version)
                })
            };
            if !satisfied {
                return Err(SupervisorError::UnsatisfiedRequirement {
                    scope_id: scope_id.to_owned(),
                    contract_id: requirement.contract_id.clone(),
                    version_range: requirement.version_range.clone(),
                });
            }
        }
        Ok(())
    }

    async fn spawn_worker(
        &self,
        lease: ModuleLease,
        entry: &str,
    ) -> Result<WorkerInstance, SupervisorError> {
        let module = lease.module();
        let instance_id = Uuid::new_v4().to_string();
        let executable = module.entry_path(entry)?;
        let log_path = self
            .inner
            .logs_dir
            .join(format!("{instance_id}.stderr.log"));
        let workspace = self.inner.logs_dir.parent().expect("logs directory has parent")
            .join("worker-workspaces").join(&instance_id);
        fs::create_dir_all(&workspace)?;
        let workspace = fs::canonicalize(workspace)?;
        let stderr = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&log_path)?;
        let provides_json = serde_json::to_string(&module.manifest.provides)
            .expect("verified module contracts serialize");
        let mut command = Command::new(executable);
        command
            .current_dir(&module.root)
            .env(WORKER_INSTANCE_ID_ENV, &instance_id)
            .env(WORKER_MODULE_ID_ENV, &module.manifest.id)
            .env(WORKER_MODULE_VERSION_ENV, &module.manifest.version)
            .env(WORKER_MODULE_DIGEST_ENV, &module.digest)
            .env(WORKER_PROVIDES_ENV, provides_json)
            .env(WORKER_WORKSPACE_ENV, &workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let input = child.stdin.take().expect("piped worker stdin is available");
        let output = child
            .stdout
            .take()
            .expect("piped worker stdout is available");
        let mut connection = WorkerConnection {
            child,
            input,
            output,
        };

        let hello = match time::timeout(
            WORKER_HANDSHAKE_TIMEOUT,
            read_worker_message::<_, WorkerHello>(&mut connection.output),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                terminate(&mut connection.child).await;
                return Err(SupervisorError::HandshakeTimeout);
            }
        };
        let mismatch = handshake_mismatch(
            &hello,
            &instance_id,
            &module.manifest.id,
            &module.manifest.version,
            &module.digest,
            &module.manifest.provides,
        );
        let accepted = mismatch.is_none();
        write_worker_message(
            &mut connection.input,
            &WorkerHostHello {
                protocol_version: WORKER_PROTOCOL_VERSION,
                generation_id: self.inner.generation_id.clone(),
                accepted,
                error: mismatch.clone(),
            },
        )
        .await?;
        if let Some(message) = mismatch {
            terminate(&mut connection.child).await;
            return Err(SupervisorError::HandshakeMismatch {
                instance_id,
                message,
            });
        }

        let health = WorkerRequest {
            id: Uuid::new_v4().to_string(),
            kind: WorkerRequestKind::Health,
            contract_id: None,
            contract_version: None,
            method: None,
            params: json!({}),
            grants: Vec::new(),
        };
        let response = call_worker(
            &instance_id,
            &mut connection,
            &health,
            WORKER_HEALTH_TIMEOUT,
        )
        .await?;
        if !response
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status == "healthy")
        {
            terminate(&mut connection.child).await;
            return Err(SupervisorError::HandshakeMismatch {
                instance_id,
                message: "health response was not healthy".to_owned(),
            });
        }

        Ok(WorkerInstance {
            instance_id,
            module_id: module.manifest.id.clone(),
            module_version: module.manifest.version.clone(),
            module_digest: module.digest.clone(),
            log_path,
            workspace,
            lifecycle: AtomicU8::new(STATE_ACTIVE),
            active_requests: AtomicUsize::new(0),
            connection: AsyncMutex::new(connection),
            _module_lease: lease,
        })
    }
}

impl WorkerInstance {
    fn lifecycle(&self) -> InstanceLifecycle {
        match self.lifecycle.load(Ordering::Acquire) {
            STATE_ACTIVE => InstanceLifecycle::Active,
            STATE_DRAINING => InstanceLifecycle::Draining,
            STATE_STOPPED => InstanceLifecycle::Stopped,
            STATE_CRASHED => InstanceLifecycle::Crashed,
            _ => unreachable!("worker lifecycle value is internal"),
        }
    }

    fn snapshot(&self) -> InstanceSnapshot {
        InstanceSnapshot {
            instance_id: self.instance_id.clone(),
            module_id: self.module_id.clone(),
            module_version: self.module_version.clone(),
            module_digest: self.module_digest.clone(),
            lifecycle: self.lifecycle(),
            active_requests: self.active_requests.load(Ordering::Acquire),
            log_path: self.log_path.clone(),
        }
    }

    async fn invoke(
        &self,
        contract_id: &str,
        contract_version: u64,
        method: &str,
        params: Value,
        grants: &[ArtifactGrant],
        deadline: Duration,
        broker: Option<&BrokerContext<'_>>,
    ) -> Result<Value, SupervisorError> {
        if self.lifecycle() != InstanceLifecycle::Active {
            return Err(SupervisorError::InstanceUnavailable(
                self.instance_id.clone(),
            ));
        }
        self.active_requests.fetch_add(1, Ordering::AcqRel);
        let _active = ActiveRequest {
            count: &self.active_requests,
        };
        if self.lifecycle() != InstanceLifecycle::Active {
            return Err(SupervisorError::InstanceUnavailable(
                self.instance_id.clone(),
            ));
        }
        let request = WorkerRequest {
            id: Uuid::new_v4().to_string(),
            kind: WorkerRequestKind::Invoke,
            contract_id: Some(contract_id.to_owned()),
            contract_version: Some(contract_version),
            method: Some(method.to_owned()),
            params,
            grants: grants
                .iter()
                .map(|grant| grant.descriptor.clone())
                .collect(),
        };
        let mut connection = self.connection.lock().await;
        match call_worker_broker(&self.instance_id, &mut connection, &request, deadline, broker).await {
            Ok(result) => Ok(result),
            Err(error @ SupervisorError::WorkerApplication { .. }) => Err(error),
            Err(error) => {
                self.lifecycle.store(STATE_CRASHED, Ordering::Release);
                terminate(&mut connection.child).await;
                Err(error)
            }
        }
    }

    async fn drain(&self) -> Result<(), SupervisorError> {
        let previous = self.lifecycle.swap(STATE_DRAINING, Ordering::AcqRel);
        if previous == STATE_STOPPED {
            return Ok(());
        }
        if previous == STATE_CRASHED {
            let mut connection = self.connection.lock().await;
            terminate(&mut connection.child).await;
            self.lifecycle.store(STATE_STOPPED, Ordering::Release);
            return Ok(());
        }
        let deadline = time::Instant::now() + WORKER_DRAIN_TIMEOUT;
        while self.active_requests.load(Ordering::Acquire) != 0 {
            if time::Instant::now() >= deadline {
                let mut connection = self.connection.lock().await;
                terminate(&mut connection.child).await;
                self.lifecycle.store(STATE_STOPPED, Ordering::Release);
                return Err(SupervisorError::DrainTimeout);
            }
            time::sleep(Duration::from_millis(10)).await;
        }
        let request = WorkerRequest {
            id: Uuid::new_v4().to_string(),
            kind: WorkerRequestKind::Shutdown,
            contract_id: None,
            contract_version: None,
            method: None,
            params: json!({}),
            grants: Vec::new(),
        };
        let mut connection = self.connection.lock().await;
        if let Err(error) = call_worker(
            &self.instance_id,
            &mut connection,
            &request,
            WORKER_HEALTH_TIMEOUT,
        )
        .await
        {
            terminate(&mut connection.child).await;
            self.lifecycle.store(STATE_STOPPED, Ordering::Release);
            return Err(error);
        }
        match time::timeout(WORKER_HEALTH_TIMEOUT, connection.child.wait()).await {
            Ok(result) => {
                result?;
            }
            Err(_) => terminate(&mut connection.child).await,
        }
        self.lifecycle.store(STATE_STOPPED, Ordering::Release);
        Ok(())
    }
}

async fn call_worker(
    instance_id: &str,
    connection: &mut WorkerConnection,
    request: &WorkerRequest,
    deadline: Duration,
) -> Result<Value, SupervisorError> {
    call_worker_broker(instance_id, connection, request, deadline, None).await
}

async fn call_worker_broker(
    instance_id: &str,
    connection: &mut WorkerConnection,
    request: &WorkerRequest,
    deadline: Duration,
    broker: Option<&BrokerContext<'_>>,
) -> Result<Value, SupervisorError> {
    if let Some(status) = connection.child.try_wait()? {
        return Err(SupervisorError::WorkerExited {
            instance_id: instance_id.to_owned(),
            status: format!(" with status {status}"),
        });
    }
    let response = time::timeout(deadline, async {
        write_worker_message(&mut connection.input, request).await?;
        let mut allowed: HashMap<String, ArtifactGrantDescriptor> = broker.map(|context| {
            context.original.iter().map(|grant| (grant.descriptor.grant_id.clone(), grant.descriptor.clone())).collect()
        }).unwrap_or_default();
        let mut produced_leases = Vec::new();
        loop {
            match read_worker_message::<_, WorkerOutbound>(&mut connection.output).await? {
                WorkerOutbound::Response(response) => break Ok::<_, SupervisorError>(response),
                WorkerOutbound::Broker { broker: callback } => {
                    let id = callback.id.clone();
                    let outcome = match broker {
                        Some(context) if callback.contract_id == "source.network" => {
                            dispatch_host_callback(context, callback).await
                        }
                        Some(context) => dispatch_engine_callback(context, callback, &mut allowed, &mut produced_leases, deadline).await,
                        None => Err(SupervisorError::InvalidBinding("callbacks are not authorized for this invocation".into())),
                    };
                    let response = match outcome {
                        Ok(result) => WorkerResponse { id, ok: true, result: Some(result), error: None },
                        Err(error) => WorkerResponse {
                            id: id.clone(), ok: false, result: None,
                            error: Some(*mintcat_sdk::worker_app_error(id, "BROKER_REJECTED", "worker.broker.rejected", json!({"message": error.to_string()}))),
                        },
                    };
                    write_worker_message(&mut connection.input, &response).await?;
                }
            }
        }
    }).await.map_err(|_| SupervisorError::RequestTimeout)??;
    if response.id != request.id {
        return Err(SupervisorError::ResponseMismatch {
            expected: request.id.clone(),
            actual: response.id,
        });
    }
    if response.ok {
        response
            .result
            .ok_or_else(|| SupervisorError::HandshakeMismatch {
                instance_id: instance_id.to_owned(),
                message: "successful response omitted result".to_owned(),
            })
    } else {
        let AppError {
            code, message_key, ..
        } = response
            .error
            .ok_or_else(|| SupervisorError::HandshakeMismatch {
                instance_id: instance_id.to_owned(),
                message: "failed response omitted error".to_owned(),
            })?;
        Err(SupervisorError::WorkerApplication { code, message_key })
    }
}

async fn dispatch_host_callback(
    context: &BrokerContext<'_>,
    callback: WorkerBrokerRequest,
) -> Result<Value, SupervisorError> {
    let manifest = &context.caller._module_lease.module().manifest;
    let required = manifest.requires.iter().any(|requirement| {
        !requirement.optional && requirement.binding_scope.as_deref() == Some("host")
            && requirement.contract_id == callback.contract_id
            && version_matches(&requirement.version_range, callback.contract_version)
    });
    let permitted = manifest.permissions.iter().any(|permission| permission.capability == "network");
    if !required || !permitted || callback.provider_digest != "host:source.network"
        || callback.contract_id != "source.network" || callback.contract_version != 1
        || callback.method != "execute" || !callback.grant_ids.is_empty() || !callback.produced.is_empty()
    {
        return Err(SupervisorError::InvalidBinding("unapproved source network callback".into()));
    }
    let service = context.host_services.lock().expect("host services poisoned")
        .get(&(callback.contract_id, callback.contract_version)).cloned()
        .ok_or_else(|| SupervisorError::InvalidBinding("host service unavailable".into()))?;
    service.execute(HostServiceCaller {
        module_id: context.caller.module_id.clone(),
        module_digest: context.caller.module_digest.clone(),
    }, &callback.method, callback.params).await
}

async fn dispatch_engine_callback(
    context: &BrokerContext<'_>,
    callback: WorkerBrokerRequest,
    allowed: &mut HashMap<String, ArtifactGrantDescriptor>,
    produced_leases: &mut Vec<ArtifactGrant>,
    deadline: Duration,
) -> Result<Value, SupervisorError> {
    let engine = context.engine.as_ref().ok_or_else(|| SupervisorError::InvalidBinding("worker has no approved engine requirement".into()))?;
    if callback.contract_id != "engine.unreal-pak" || callback.contract_version != 1
        || callback.provider_digest != engine.module_digest
        || !matches!(callback.method.as_str(), "inspect" | "conflicts" | "build" | "verify") {
        return Err(SupervisorError::InvalidBinding("callback does not name the immutable required engine contract/provider/method".into()));
    }
    for descriptor in callback.produced {
        if let Some(original) = allowed.get(&descriptor.grant_id) {
            if original != &descriptor {
                return Err(SupervisorError::InvalidBinding("callback altered an authenticated grant".into()));
            }
            continue;
        }
        let grant = context.artifacts.grant_produced(context.producer_workspace, &descriptor)?;
        // The ID is local to this invocation. Paths and bytes have been host
        // authenticated; preserve the reference used in engine method params.
        let mut authenticated = grant.descriptor.clone();
        authenticated.grant_id = descriptor.grant_id.clone();
        allowed.insert(authenticated.grant_id.clone(), authenticated);
        produced_leases.push(grant);
    }
    let descriptors = callback.grant_ids.iter().map(|id| {
        allowed.get(id).cloned().ok_or_else(|| SupervisorError::InvalidBinding("callback names an ungranted artifact".into()))
    }).collect::<Result<Vec<_>, _>>()?;
    let envelope = WorkerRequest {
        id: callback.id, kind: WorkerRequestKind::Invoke, contract_id: Some(callback.contract_id.clone()),
        contract_version: Some(callback.contract_version), method: Some(callback.method.clone()),
        params: callback.params.clone(), grants: descriptors,
    };
    let grants = envelope.grants.iter().map(|descriptor| {
        ArtifactGrant::from_worker_request(&envelope, &descriptor.grant_id)
    }).collect::<Result<Vec<_>, _>>()?;
    let mut result = Box::pin(engine.invoke(
        &callback.contract_id, callback.contract_version, &callback.method,
        callback.params, &grants, deadline, None,
    )).await?;
    if callback.method == "build" {
        let descriptor: ArtifactGrantDescriptor = serde_json::from_value(result.get("grant")
            .cloned().ok_or_else(|| SupervisorError::InvalidBinding("engine build omitted produced descriptor".into()))?)
            .map_err(WorkerProtocolError::from)?;
        let grant = context.artifacts.grant_produced(&engine.workspace, &descriptor)?;
        result["grant"] = serde_json::to_value(&grant.descriptor).map_err(WorkerProtocolError::from)?;
        allowed.insert(grant.descriptor.grant_id.clone(), grant.descriptor.clone());
        produced_leases.push(grant);
    }
    Ok(result)
}

fn handshake_mismatch(
    hello: &WorkerHello,
    instance_id: &str,
    module_id: &str,
    module_version: &str,
    module_digest: &str,
    provides: &[mintcat_sdk::ProvidedContract],
) -> Option<String> {
    if hello.protocol_version != WORKER_PROTOCOL_VERSION {
        Some(format!(
            "protocol {} is not {}",
            hello.protocol_version, WORKER_PROTOCOL_VERSION
        ))
    } else if hello.instance_id != instance_id {
        Some("instance id differs".to_owned())
    } else if hello.module_id != module_id {
        Some("module id differs".to_owned())
    } else if hello.module_version != module_version {
        Some("module version differs".to_owned())
    } else if hello.module_digest != module_digest {
        Some("module digest differs".to_owned())
    } else if hello.provides != provides {
        Some("provided contracts differ".to_owned())
    } else {
        None
    }
}

fn version_matches(version_range: &str, version: u64) -> bool {
    let value = version_range.trim();
    value == "*"
        || value
            .strip_prefix('=')
            .unwrap_or(value)
            .parse::<u64>()
            .is_ok_and(|required| required == version)
}

async fn terminate(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill().await;
    }
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use mintcat_sdk::{ProvidedContract, WorkerHello, WORKER_PROTOCOL_VERSION};

    use super::{handshake_mismatch, version_matches};

    #[test]
    fn contract_version_ranges_are_explicit() {
        assert!(version_matches("1", 1));
        assert!(version_matches("=2", 2));
        assert!(version_matches("*", 99));
        assert!(!version_matches("^1", 1));
        assert!(!version_matches("2", 1));
    }

    #[test]
    fn handshake_identity_is_bound_to_the_verified_manifest() {
        let provides = vec![ProvidedContract {
            contract_id: "fixture.compute".to_owned(),
            version: 1,
            cardinality: Some("single".to_owned()),
        }];
        let mut hello = WorkerHello {
            protocol_version: WORKER_PROTOCOL_VERSION,
            instance_id: "instance-1".to_owned(),
            module_id: "fixture.worker".to_owned(),
            module_version: "1.0.0".to_owned(),
            module_digest: format!("sha256:{}", "1".repeat(64)),
            provides: provides.clone(),
        };
        assert_eq!(
            handshake_mismatch(
                &hello,
                "instance-1",
                "fixture.worker",
                "1.0.0",
                &hello.module_digest,
                &provides,
            ),
            None
        );
        hello.module_digest = format!("sha256:{}", "2".repeat(64));
        assert_eq!(
            handshake_mismatch(
                &hello,
                "instance-1",
                "fixture.worker",
                "1.0.0",
                &format!("sha256:{}", "1".repeat(64)),
                &provides,
            )
            .as_deref(),
            Some("module digest differs")
        );
    }
}
