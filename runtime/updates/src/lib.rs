mod model;
mod owner;
mod network;
mod lifecycle;
mod leases;
mod library_owner;
mod trust;
mod inventory;
mod ui;
mod closure;
pub use ui::{UiArtifactAuthority, UiModuleAuthority, UiCallAdmission};
pub use library_owner::LibraryStateOwner;
pub use model::*;
pub use owner::*;

use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};
use mintcat_feature_backup::{MaintenanceAdmission, MaintenanceGate, MaintenanceMarker, MaintenancePhase};
use mintcat_runtime_artifacts::{ArtifactError, ArtifactStore, GarbageCollectionReport};
use mintcat_runtime_kernel::{KernelDatabase, KernelDatabaseError, RuntimeLayout};
use mintcat_runtime_release::{LocalRepository, RepositoryBundle, RepositoryVerifier, VerificationPolicy, VerifiedScopeRelease};
use mintcat_runtime_supervisor::{ModuleSupervisor, SupervisorError, HostService};
use mintcat_sdk::ScopedProviderBinding;
use rusqlite::TransactionBehavior;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("updates database: {0}")] Database(#[from] KernelDatabaseError),
    #[error("updates SQLite: {0}")] Sqlite(#[from] rusqlite::Error),
    #[error("updates JSON: {0}")] Json(#[from] serde_json::Error),
    #[error("updates I/O: {0}")] Io(#[from] std::io::Error),
    #[error("signed release: {0}")] Release(#[from] mintcat_runtime_release::ReleaseError),
    #[error("artifact: {0}")] Artifact(#[from] ArtifactError),
    #[error("worker: {0}")] Worker(#[from] SupervisorError),
    #[error("transfer: {0}")] Transfer(#[from] mintcat_runtime_transfers::TransferError),
    #[error("network: {0}")] Network(#[from] mintcat_runtime_broker::BrokerError),
    #[error("maintenance: {0}")] Maintenance(#[from] mintcat_feature_backup::BackupError),
    #[error("update blocked: {0}")] Blocked(String),
    #[error("waiting for safe point: {0}")] Busy(String),
    #[error("stale UI session: {0}")] StaleUiSession(String),
    #[error("artifact revoked: {0}")] ArtifactRevoked(String),
    #[error("candidate metadata expired or clock moved backwards (verified {verified_at}, expires {expires_at}, now {now})")]
    MetadataExpired { verified_at: u64, expires_at: u64, now: u64 },
}
pub type Result<T, E = UpdateError> = std::result::Result<T, E>;
fn require(ok: bool, message: impl Into<String>) -> Result<()> { if ok { Ok(()) } else { Err(UpdateError::Blocked(message.into())) } }
fn id(prefix: &str) -> String { format!("{prefix}-{}", Uuid::new_v4()) }

#[derive(Clone)]
pub struct UpdatesCoordinator { inner: Arc<Inner> }
struct Inner {
    layout: RuntimeLayout,
    database: KernelDatabase,
    artifacts: ArtifactStore,
    verifier: RepositoryVerifier,
    policy: VerificationPolicy,
    installation_id: String,
    supervisors: Mutex<HashMap<String, ModuleSupervisor>>,
    owners: Mutex<BTreeMap<String, Arc<dyn StateOwner>>>,
    maintenance: Mutex<Option<(String, MaintenanceGate)>>,
    prepared: Mutex<HashSet<String>>,
    native_ready: Mutex<HashSet<(String, String)>>,
    host_services: Mutex<BTreeMap<(String, u64), Arc<dyn HostService>>>,
    recovered: AtomicBool,
    startup_readonly: AtomicBool,
    leases_reconciled: AtomicBool,
    lifecycle: tokio::sync::Mutex<()>,
    ui_fence: Arc<tokio::sync::RwLock<()>>,
}
impl UpdatesCoordinator {
    pub fn open(layout: RuntimeLayout, database: KernelDatabase, artifacts: ArtifactStore, verifier: RepositoryVerifier, policy: VerificationPolicy, installation_id: String) -> Result<Self> {
        require(layout.initialized && !installation_id.is_empty(), "initialized layout and stable installation identity required")?;
        database.apply_migration("runtime.updates", 1, "CREATE TABLE update_state(id INTEGER PRIMARY KEY CHECK(id=1), state_json TEXT NOT NULL); CREATE TABLE update_events(seq INTEGER PRIMARY KEY AUTOINCREMENT, event TEXT NOT NULL);")?;
        database.connection()?.execute("INSERT OR IGNORE INTO update_state(id,state_json) VALUES(1,?1)", [serde_json::to_string(&UpdateState::default())?])?;
        Ok(Self { inner: Arc::new(Inner { layout, database, artifacts, verifier, policy, installation_id, supervisors: Mutex::new(HashMap::new()), owners: Mutex::new(BTreeMap::new()), maintenance: Mutex::new(None), prepared: Mutex::new(HashSet::new()), native_ready: Mutex::new(HashSet::new()), host_services: Mutex::new(BTreeMap::new()), recovered: AtomicBool::new(false), startup_readonly: AtomicBool::new(false), leases_reconciled: AtomicBool::new(false), lifecycle: tokio::sync::Mutex::new(()), ui_fence: Arc::new(tokio::sync::RwLock::new(())) }) })
    }
    fn read_state(&self) -> Result<UpdateState> {
        let json: String = self.inner.database.connection()?.query_row("SELECT state_json FROM update_state WHERE id=1", [], |row| row.get(0))?;
        Ok(serde_json::from_str(&json)?)
    }
    fn change<T>(&self, event: &str, f: impl FnOnce(&mut UpdateState) -> Result<T>) -> Result<T> {
        let mut connection = self.inner.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let json: String = transaction.query_row("SELECT state_json FROM update_state WHERE id=1", [], |row| row.get(0))?;
        let mut state: UpdateState = serde_json::from_str(&json)?;
        let result = f(&mut state)?;
        let json = serde_json::to_string(&state)?;
        transaction.execute("UPDATE update_state SET state_json=?1 WHERE id=1", [&json])?;
        transaction.execute("INSERT INTO update_events(event) VALUES(?1)", [event])?;
        transaction.commit()?;
        Ok(result)
    }
    fn available(&self) -> Result<()> {
        require(self.inner.recovered.load(Ordering::Acquire), "startup recovery must finish before admissions")?;
        require(self.read_state()?.recovery_required.is_none(), "runtime is in read-only rescue; inspect update failures before enabling mutations")
    }
    pub fn register_state_owner(&self, owner: Arc<dyn StateOwner>) -> Result<()> {
        let mut owners = self.inner.owners.lock().expect("owner registry poisoned");
        require(!owners.contains_key(owner.owner_id()), "state owner already registered")?;
        owners.insert(owner.owner_id().to_owned(), owner); Ok(())
    }
    pub fn register_host_service(&self, contract_id: &str, version: u64, service: Arc<dyn HostService>) -> Result<()> {
        require(contract_id == "source.network" && version == 1, "unapproved host service contract")?;
        let mut services = self.inner.host_services.lock().expect("host service registry poisoned");
        let key = (contract_id.into(), version);
        require(!services.contains_key(&key), "host service already registered")?;
        services.insert(key, service); Ok(())
    }
    pub fn environment(&self) -> Result<EnvironmentSnapshot> { Ok(self.read_state()?.environment) }
    pub fn operation(&self, operation_id: &str) -> Result<UpdateOperation> { self.read_state()?.operations.remove(operation_id).ok_or_else(|| UpdateError::Blocked("unknown update operation".into())) }
    pub fn operations(&self) -> Result<Vec<UpdateOperation>> { Ok(self.read_state()?.operations.into_values().collect()) }
    pub fn scope_revision(&self, scope: &str) -> Result<u64> { Ok(self.read_state()?.revisions.get(scope).copied().unwrap_or(0)) }

    fn begin(&self, bundle: RepositoryBundle, verified: VerifiedScopeRelease, expected: u64, now: u64) -> Result<UpdateOperation> {
        self.available()?;
        verified.require_fresh(now)?;
        for record in verified.package_records().values() { inventory::signed_game(record)?; }
        let release = verified.release();
        require(verified.cohort().includes(&release.scope_id, &release.channel, &self.inner.installation_id)?, "installation is outside signed release cohort")?;
        let metadata_digest = mintcat_runtime_release::sha256(&serde_json::to_vec(verified.targets())?);
        let key = format!("{}:{}:{}", release.scope_id, release.channel, release.target);
        self.change("metadata-verified", |state| {
            for record in verified.package_records().values().filter(|p| p.manifest.components.iter().any(|c| c.state_model != "stateless")) {
                require(!state.environment.scopes.iter().any(|(scope, generation)| scope != &release.scope_id && generation.targets.signed.packages.values().any(|p| p.manifest.id == record.manifest.id)),
                    "a state owner cannot be forked into independent scopes; use an explicit platform singleton requirement")?;
            }
            let previous = state.environment.scopes.get(&release.scope_id).cloned();
            require(state.revisions.get(&release.scope_id).copied().unwrap_or(0) == expected, "expectedScopeRevision conflict")?;
            require(!state.operations.values().any(|op| op.scope_id == release.scope_id && !matches!(op.phase, UpdatePhase::Confirmed | UpdatePhase::Rejected | UpdatePhase::RolledBack)), "scope already has a pending operation")?;
            if let Some((sequence, digest)) = state.high_water.get(&key) {
                require(release.sequence >= *sequence && (release.sequence != *sequence || *digest == metadata_digest), "scope sequence rollback/equivocation")?;
            }
            require(!state.operations.values().any(|op| op.quarantined && op.generation.targets.signed.release.release_id == release.release_id && op.scope_id == release.scope_id), "failed release is quarantined; publish a new signed release")?;
            require(!verified.package_records().keys().any(|d| state.revoked.contains(d)), "release contains a revoked package")?;
            if let Some(previous) = &previous {
                require(previous.targets.signed.release.channel == release.channel || state.authorized_channels.get(&release.scope_id) == Some(&release.channel), "channel switch requires explicit authorize_channel")?;
                if previous.targets.signed.release.channel == release.channel { require(release.sequence > previous.targets.signed.release.sequence, "release sequence must advance current scope")?; }
            }
            state.high_water.insert(key, (release.sequence, metadata_digest));
            let operation = UpdateOperation { operation_id: id("update"), scope_id: release.scope_id.clone(), expected_scope_revision: expected,
                generation: ScopeGeneration { generation_id: id("generation"), revision: expected.checked_add(1).ok_or_else(|| UpdateError::Blocked("scope revision overflow".into()))?, targets: verified.targets().clone(), verified_root: verified.verified_root().envelope().clone(), confirmed: false, new_writes: false },
                previous, phase: UpdatePhase::MetadataVerified, failure: None, verified_at: now, metadata_expires_at: verified.metadata_expires_at(), quarantined: false, bundle, backups: BTreeMap::new(), drained: BTreeSet::new(), migrated: BTreeSet::new(), barrier: None };
            state.operations.insert(operation.operation_id.clone(), operation.clone()); Ok(operation)
        })
    }
    fn select(&self, bundle: &RepositoryBundle, scope: &str, channel: &str, now: u64) -> Result<VerifiedScopeRelease> {
        let result = self.inner.verifier.verify_scope(bundle, scope, channel, &self.inner.policy, now);
        if let Err(error) = &result { self.change(&format!("metadata-rejected:{scope}:{channel}:{error}"), |_| Ok(()))?; }
        Ok(result?)
    }
    fn phase(&self, operation_id: &str, phase: UpdatePhase) -> Result<UpdateOperation> {
        self.change("update-phase", |s| { let op = s.operations.get_mut(operation_id).ok_or_else(|| UpdateError::Blocked("unknown update operation".into()))?; op.phase = phase; Ok(op.clone()) })
    }
    fn reject(&self, operation_id: &str, error: &UpdateError) -> Result<()> {
        let maintenance_held = self.inner.maintenance.lock().expect("maintenance mutex poisoned").as_ref().is_some_and(|(op, _)| op == operation_id);
        self.change("update-rejected", |s| { let op = s.operations.get_mut(operation_id).ok_or_else(|| UpdateError::Blocked("unknown update operation".into()))?; require(!matches!(op.phase, UpdatePhase::Probation | UpdatePhase::Confirmed | UpdatePhase::RolledBack), "published operation must use scoped rollback, not rejection")?; op.quarantined = !matches!(error, UpdateError::MetadataExpired { .. } | UpdateError::Io(_) | UpdateError::Artifact(ArtifactError::Io(_))); op.phase = if !maintenance_held && op.drained.is_empty() && op.backups.is_empty() { UpdatePhase::Rejected } else { s.recovery_required = Some(error.to_string()); s.recovery_operation = Some(operation_id.into()); UpdatePhase::RecoveryRequired }; op.failure = Some(error.to_string()); Ok(()) })
    }
    pub async fn stage_local(&self, repository: &LocalRepository, scope: &str, channel: &str, expected_scope_revision: u64, now: u64) -> Result<UpdateOperation> {
        let coordinator = self.clone();
        let repository = repository.clone();
        let scope = scope.to_owned();
        let channel = channel.to_owned();
        tokio::task::spawn_blocking(move || {
            coordinator.stage_local_blocking(&repository, &scope, &channel, expected_scope_revision, now)
        }).await.map_err(|error| UpdateError::Blocked(format!("local staging task failed: {error}")))?
    }
    fn stage_local_blocking(&self, repository: &LocalRepository, scope: &str, channel: &str, expected_scope_revision: u64, now: u64) -> Result<UpdateOperation> {
        let bundle = repository.current_bundle()?.ok_or_else(|| UpdateError::Blocked("repository has no signed current bundle".into()))?;
        let verified = self.select(&bundle, scope, channel, now)?;
        let operation = self.begin(bundle, verified, expected_scope_revision, now)?;
        let result = (|| {
            self.phase(&operation.operation_id, UpdatePhase::Resolved)?;
            self.phase(&operation.operation_id, UpdatePhase::Downloading)?;
            let root = mintcat_runtime_release::trust_pinned_root(operation.generation.verified_root.clone(), now)?;
            let artifacts = self.generation_store(&operation.generation)?;
            for record in operation.generation.targets.signed.packages.values() {
                let path = repository.package_path(&record.digest)?;
                require(mintcat_runtime_release::verify_package(&path, &root, &self.inner.policy)? == *record, "package differs from exact signed closure")?;
                let installed = artifacts.publish_module(&path.join("payload"), &record.manifest, &record.signature)?;
                require(installed.digest == record.digest, "installed package digest mismatch")?;
            }
            self.phase(&operation.operation_id, UpdatePhase::Staged)
        })();
        if let Err(error) = &result { self.reject(&operation.operation_id, error)?; }
        result
    }
    fn owners_for(&self, operation: &UpdateOperation) -> Result<Vec<(Arc<dyn StateOwner>, u64, u64)>> {
        let owners = self.inner.owners.lock().expect("owner registry poisoned");
        let mut result = Vec::new();
        for record in operation.generation.targets.signed.packages.values() {
            if !record.manifest.components.iter().any(|c| c.state_model != "stateless") { continue; }
            let old = operation.previous.as_ref().and_then(|generation| generation.targets.signed.packages.values().find(|previous| previous.manifest.id == record.manifest.id && previous.manifest.components.iter().any(|component| component.state_model != "stateless")));
            let owner = owners.get(&record.manifest.id).ok_or_else(|| UpdateError::Blocked(format!("state owner {} has no drain/consistent-backup/migration/read-only-health adapter; schema {} cannot be activated", record.manifest.id, record.manifest.state.schema_version)))?;
            owner.supports(old.map(|p| &p.manifest), &record.manifest)?;
            result.push((owner.clone(), old.map_or(record.manifest.state.schema_version, |p| p.manifest.state.schema_version), record.manifest.state.schema_version));
        }
        if let Some(previous) = &operation.previous {
            for old in previous.targets.signed.packages.values().filter(|record| record.manifest.components.iter().any(|component| component.state_model != "stateless")) {
                if operation.generation.targets.signed.packages.values().any(|record| record.manifest.id == old.manifest.id && record.manifest.components.iter().any(|component| component.state_model != "stateless")) { continue; }
                let owner = owners.get(&old.manifest.id).ok_or_else(|| UpdateError::Blocked(format!("removed state owner {} needs an actual drain/deactivation adapter", old.manifest.id)))?;
                owner.supports(Some(&old.manifest), &old.manifest)?;
                result.push((owner.clone(), old.manifest.state.schema_version, old.manifest.state.schema_version));
            }
        }
        Ok(result)
    }
    async fn start_generation(&self, generation: &ScopeGeneration) -> Result<ModuleSupervisor> {
        self.verify_offline(generation)?;
        for record in generation.targets.signed.packages.values() {
            for component in record.manifest.components.iter().filter(|c| c.kind == "service") {
                if component.state_model == "stateless" {
                    require(component.runtime == "worker", "stateless service has no actual worker runtime")?;
                } else {
                    require(self.inner.native_ready.lock().expect("native owner readiness poisoned").contains(&(generation.generation_id.clone(), record.manifest.id.clone()))
                        && self.inner.owners.lock().expect("owner registry poisoned").get(&record.manifest.id).is_some_and(|owner| owner.handles_native(&record.manifest, component)), "stateful service needs exact observed native code readiness, not merely a snapshot adapter")?;
                }
            }
        }
        let current = self.read_state()?;
        let mut environment = EnvironmentSnapshot { generation_id: generation.generation_id.clone(), scopes: BTreeMap::from([(generation.targets.signed.release.scope_id.clone(), generation.clone())]), disabled_ui: BTreeSet::new() };
        for required in &generation.targets.signed.release.singleton_requirements {
            let scope = required.binding_scope.as_deref().unwrap_or("platform");
            if scope == generation.targets.signed.release.scope_id { continue; }
            if let Some(singleton) = current.environment.scopes.get(scope) {
                let providers: Vec<_> = singleton.targets.signed.release.provider_bindings.iter().filter(|b| b.contract_id == required.contract_id && mintcat_runtime_release::version_matches(&required.version_range, b.contract_version).unwrap_or(false)).collect();
                for provider in providers { if current.revoked.contains(&provider.provider_digest) { return Err(UpdateError::ArtifactRevoked(provider.provider_digest.clone())); } }
                environment.scopes.insert(scope.into(), singleton.clone());
            }
        }
        self.build_closure_supervisor(&environment, false, None).await
    }
    pub async fn prepare(&self, operation_id: &str, now: u64) -> Result<UpdateOperation> {
        let started = std::time::Instant::now();
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.available()?;
        let operation = self.operation(operation_id)?;
        require(matches!(operation.phase, UpdatePhase::Staged | UpdatePhase::WaitingForSafePoint), "prepare requires staged verified closure or waiting safe point")?;
        let result: Result<UpdateOperation> = async {
            check_freshness(&operation, now)?;
            // Capability checks happen before draining anything. Unsupported migrations leave the old owner writable.
            let owners = self.owners_for(&operation)?;
            self.phase(operation_id, UpdatePhase::WaitingForSafePoint)?;
            if !owners.is_empty() {
                let gate = MaintenanceGate::exclusive(&self.inner.layout, false)?;
                gate.mark(&MaintenanceMarker { format_version: 1, owner_id: "runtime.updates".into(), operation_id: Some(operation_id.into()), phase: MaintenancePhase::PreparingMigration, snapshot_digest: String::new() })?;
                *self.inner.maintenance.lock().expect("maintenance mutex poisoned") = Some((operation_id.into(), gate));
                // No new task/service lease can cross this exclusive admission boundary.
                let state = self.read_state()?;
                for (owner, _, _) in &owners {
                    let old_digest = operation.previous.as_ref().and_then(|g| g.targets.signed.packages.values().find(|p| p.manifest.id == owner.owner_id())).map(|p| &p.digest);
                    if old_digest.is_some_and(|digest| state.leases.values().any(|lease| matches!(lease.kind, LeaseKind::Task | LeaseKind::Service) && lease.digests.contains(digest) && (lease.running || lease.expires_at.is_none_or(|expires| expires > now)))) {
                        return Err(UpdateError::Busy(format!("state owner {} remains pinned by an unfinished task, plan, or service", owner.owner_id())));
                    }
                }
                // Drain every owner before taking/migrating any owner's backup.
                for (owner, _, _) in &owners {
                    self.change("owner-drain-started", |s| { s.operations.get_mut(operation_id).expect("operation exists").drained.insert(owner.owner_id().into()); Ok(()) })?;
                    if let Err(error) = owner.drain().await {
                        if matches!(error, UpdateError::Busy(_)) {
                            self.change("owner-drain-busy", |s| { s.operations.get_mut(operation_id).expect("operation exists").drained.remove(owner.owner_id()); Ok(()) })?;
                        }
                        return Err(error);
                    }
                }
                for (owner, old, new) in owners {
                    let candidate_present = operation.generation.targets.signed.packages.values().any(|record| record.manifest.id == owner.owner_id() && record.manifest.components.iter().any(|component| component.state_model != "stateless"));
                    let previous_present = operation.previous.as_ref().is_some_and(|generation| generation.targets.signed.packages.values().any(|record| record.manifest.id == owner.owner_id() && record.manifest.components.iter().any(|component| component.state_model != "stateless")));
                    // The sole approved first-install reader is selected read-only.
                    // Never initialize an unavailable owner merely to take its backup.
                    if candidate_present && !previous_present { owner.prepare_code(&operation.generation, self.generation_store(&operation.generation)?).await?; }
                    let backup = owner.backup(old, new).await?;
                    require(backup.owner_id == owner.owner_id() && backup.old_schema == old && backup.new_schema == new, "owner backup schema/identity mismatch")?;
                    self.change("owner-backed-up", |s| { s.operations.get_mut(operation_id).expect("operation exists").backups.insert(owner.owner_id().into(), backup.clone()); Ok(()) })?;
                    if !candidate_present {
                        owner.deactivate_code(operation.previous.as_ref().expect("removed owner has previous generation")).await?;
                        continue;
                    }
                    if previous_present { owner.prepare_code(&operation.generation, self.generation_store(&operation.generation)?).await?; }
                    if old != new {
                        // Mark before migration: crash during migration must restore, not assume no changes.
                        self.change("owner-migration-started", |s| { s.operations.get_mut(operation_id).expect("operation exists").migrated.insert(owner.owner_id().into()); Ok(()) })?;
                        owner.migrate(&backup).await?;
                    }
                    owner.read_only_health().await?;
                    self.inner.native_ready.lock().expect("native owner readiness poisoned").insert((operation.generation.generation_id.clone(), owner.owner_id().into()));
                }
                let maintenance = self.inner.maintenance.lock().expect("maintenance mutex poisoned");
                maintenance.as_ref().expect("gate held").1.mark(&MaintenanceMarker { format_version: 1, owner_id: "runtime.updates".into(), operation_id: Some(operation_id.into()), phase: MaintenancePhase::MigratedReadOnly, snapshot_digest: mintcat_runtime_release::sha256(&serde_json::to_vec(&self.operation(operation_id)?.backups)?) })?;
            }
            let supervisor = self.start_generation(&operation.generation).await?;
            if let Err(error) = check_freshness(&operation, now.saturating_add(started.elapsed().as_secs())) {
                let _ = supervisor.shutdown_all().await;
                return Err(error);
            }
            self.inner.supervisors.lock().expect("supervisor registry poisoned").insert(operation.generation.generation_id.clone(), supervisor);
            self.inner.prepared.lock().expect("prepared registry poisoned").insert(operation_id.into());
            self.change("prepared", |s| {
                let sessions = s.leases.values().filter(|l| l.kind == LeaseKind::Ui && l.running).map(|l| (l.owner_id.clone(), None)).collect();
                let expected_generation_id = s.environment.generation_id.clone();
                let op = s.operations.get_mut(operation_id).expect("operation exists");
                op.barrier = Some(UiBarrier { barrier_id: id("ui-barrier"), affected_module_ids: affected_modules(op.previous.as_ref(), &op.generation), scope_id: op.scope_id.clone(), expected_generation_id, new_generation_id: op.generation.generation_id.clone(), runtime_changed: runtime_changed(op.previous.as_ref(), &op.generation), participants: sessions });
                op.phase = UpdatePhase::Prepared; Ok(op.clone())
            })
        }.await;
        if let Err(error) = &result {
            let busy = matches!(error, UpdateError::Busy(_) | UpdateError::Maintenance(mintcat_feature_backup::BackupError::Busy(_)));
            let has_drained = !self.operation(operation_id)?.drained.is_empty();
            if busy && !has_drained {
                let owns_gate = self.inner.maintenance.lock().expect("maintenance mutex poisoned").as_ref().is_some_and(|(op, _)| op == operation_id);
                if owns_gate { self.clear_maintenance(operation_id)?; }
                self.change("waiting-for-safe-point", |s| { let op = s.operations.get_mut(operation_id).expect("operation exists"); op.phase = UpdatePhase::WaitingForSafePoint; op.failure = Some(error.to_string()); Ok(()) })?;
            } else {
                self.reject(operation_id, error)?;
                let recovery = self.operation(operation_id)?.phase == UpdatePhase::RecoveryRequired;
                drop(_lifecycle);
                if recovery { self.rollback(operation_id, &error.to_string()).await?; }
                if busy { self.change("waiting-for-safe-point", |s| { let op = s.operations.get_mut(operation_id).expect("operation exists"); op.phase = UpdatePhase::WaitingForSafePoint; op.quarantined = false; op.failure = Some(error.to_string()); op.drained.clear(); op.backups.clear(); op.migrated.clear(); Ok(()) })?; }
            }
        }
        result
    }
    pub fn activate(&self, operation_id: &str, now: u64) -> Result<UpdateOperation> {
        let _publication = self.publication_guard()?;
        self.activate_inner(operation_id, now)
    }
    fn activate_inner(&self, operation_id: &str, now: u64) -> Result<UpdateOperation> {
        self.available()?;
        let candidate = self.operation(operation_id)?;
        require(candidate.phase == UpdatePhase::Prepared, "activation requires durable prepared")?;
        if let Err(error) = check_freshness(&candidate, now) { self.reject(operation_id, &error)?; return Err(error); }
        require(self.inner.prepared.lock().expect("prepared registry poisoned").contains(operation_id), "worker readiness must be observed in this runtime; prepare again after recovery")?;
        self.change("probation", |s| {
            let operation = s.operations.get(operation_id).ok_or_else(|| UpdateError::Blocked("unknown operation".into()))?.clone();
            require(operation.phase == UpdatePhase::Prepared, "activation requires durable prepared")?;
            require(operation.barrier.as_ref().is_some_and(UiBarrier::disposed), "UI generation barrier waiting for snapshot/dirty/dispose or explicit WebView rebuild")?;
            require(!operation.generation.digests().iter().any(|d| s.revoked.contains(d)), "candidate revoked before publication")?;
            let revision = s.revisions.get(&operation.scope_id).copied().unwrap_or(0);
            require(revision == operation.expected_scope_revision, "expectedScopeRevision conflict against latest environment")?;
            s.revisions.insert(operation.scope_id.clone(), operation.generation.revision);
            s.environment.scopes.insert(operation.scope_id.clone(), operation.generation.clone());
            s.environment.generation_id = operation.generation.generation_id.clone();
            if let Some(previous) = &operation.previous {
                let lease = DurableLease { lease_id: format!("rollback:{operation_id}"), owner_id: operation_id.into(), kind: LeaseKind::Rollback, environment: EnvironmentSnapshot { generation_id: previous.generation_id.clone(), scopes: BTreeMap::from([(operation.scope_id.clone(), previous.clone())]), disabled_ui: s.environment.disabled_ui.clone() }, digests: previous.digests(), expires_at: None, running: false };
                s.leases.insert(lease.lease_id.clone(), lease);
            }
            let op = s.operations.get_mut(operation_id).expect("operation exists"); op.phase = UpdatePhase::Probation; Ok(op.clone())
        })
    }
    pub async fn confirm(&self, operation_id: &str, now: u64) -> Result<UpdateOperation> {
        let started = std::time::Instant::now();
        let _lifecycle = self.inner.lifecycle.lock().await;
        let operation = self.operation(operation_id)?;
        require(operation.phase == UpdatePhase::Probation && operation.barrier.as_ref().is_some_and(UiBarrier::ready), "confirmation waits for worker health and all participating UI ready")?;
        if let Err(error) = check_freshness(&operation, now) {
            self.change("candidate-metadata-expired", |s| { let op = s.operations.get_mut(operation_id).expect("operation exists"); op.failure = Some(error.to_string()); op.quarantined = false; Ok(()) })?;
            drop(_lifecycle); self.rollback(operation_id, &error.to_string()).await?; return Err(error);
        }
        let supervisor = self.supervisor_for_generation(&operation.scope_id, &operation.generation.generation_id)?.ok_or_else(|| UpdateError::Blocked("probation worker realm missing".into()))?;
        if let Err(error) = supervisor.health_check().await {
            drop(_lifecycle);
            self.rollback(operation_id, &error.to_string()).await?;
            return Err(error.into());
        }
        for (owner, _, _) in self.owners_for(&operation)? {
            if !operation.generation.targets.signed.packages.values().any(|record| record.manifest.id == owner.owner_id() && record.manifest.components.iter().any(|component| component.state_model != "stateless")) { continue; }
            if let Err(error) = owner.read_only_health().await {
                drop(_lifecycle); self.rollback(operation_id, &error.to_string()).await?; return Err(error);
            }
        }
        if let Err(error) = check_freshness(&operation, now.saturating_add(started.elapsed().as_secs())) {
            self.change("metadata-expired-during-readiness", |state| { let operation = state.operations.get_mut(operation_id).expect("operation exists"); operation.failure = Some(error.to_string()); operation.quarantined = false; Ok(()) })?;
            drop(_lifecycle); self.rollback(operation_id, &error.to_string()).await?; return Err(error);
        }
        // Persist confirmed routing before opening owners; any crash is recovered read-only first.
        let confirmed = self.change("confirmed", |s| {
            let generation = s.environment.scopes.get_mut(&operation.scope_id).ok_or_else(|| UpdateError::Blocked("scope missing".into()))?;
            require(generation.generation_id == operation.generation.generation_id, "scope advanced beyond probation")?;
            generation.confirmed = true;
            s.scope_blockers.remove(&operation.scope_id);
            let op = s.operations.get_mut(operation_id).expect("operation exists"); op.generation.confirmed = true; op.phase = UpdatePhase::Confirmed; Ok(op.clone())
        })?;
        for (owner, _, _) in self.owners_for(&operation)? {
            if !operation.generation.targets.signed.packages.values().any(|record| record.manifest.id == owner.owner_id() && record.manifest.components.iter().any(|component| component.state_model != "stateless")) { continue; }
            if let Err(error) = owner.open_writes().await {
                drop(_lifecycle);
                self.rollback(operation_id, &error.to_string()).await?;
                return Err(error);
            }
        }
        self.clear_maintenance(operation_id)?;
        Ok(confirmed)
    }
    fn clear_maintenance(&self, operation_id: &str) -> Result<()> {
        let mut guard = self.inner.maintenance.lock().expect("maintenance mutex poisoned");
        if let Some((owner_op, gate)) = guard.as_ref() {
            require(owner_op == operation_id, "another operation owns maintenance")?;
            // A compatible rollback after confirmation reacquires the gate without
            // a migration marker. Clear only a marker owned by this operation.
            if let Some(marker) = gate.read()? {
                require(marker.owner_id == "runtime.updates" && marker.operation_id.as_deref() == Some(operation_id), "maintenance marker belongs to another operation")?;
                gate.clear()?;
            }
        }
        *guard = None; Ok(())
    }
    pub fn supervisor_for_generation(&self, scope: &str, generation: &str) -> Result<Option<ModuleSupervisor>> {
        let s = self.read_state()?;
        let valid = s.environment.scopes.get(scope).is_some_and(|g| g.generation_id == generation) || s.operations.values().any(|op| op.scope_id == scope && (op.generation.generation_id == generation || op.previous.as_ref().is_some_and(|p| p.generation_id == generation))) || s.leases.values().any(|l| l.environment.scopes.get(scope).is_some_and(|g| g.generation_id == generation));
        require(valid, "scope/generation is not an immutable known closure")?;
        Ok(self.inner.supervisors.lock().expect("supervisor registry poisoned").get(generation).cloned())
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.recovered.store(false, Ordering::Release);
        let supervisors = std::mem::take(&mut *self.inner.supervisors.lock().expect("supervisor registry poisoned"));
        let mut failure = None;
        for supervisor in supervisors.into_values() {
            if let Err(error) = supervisor.shutdown_all().await { if failure.is_none() { failure = Some(error); } }
        }
        match failure { Some(error) => Err(error.into()), None => Ok(()) }
    }
}
fn runtime_changed(old: Option<&ScopeGeneration>, new: &ScopeGeneration) -> bool {
    fn runtime(generation: &ScopeGeneration) -> BTreeSet<&str> { generation.targets.signed.packages.values().filter(|p| p.manifest.contributes.get("sharedUiRuntime").and_then(Value::as_bool) == Some(true)).map(|p| p.digest.as_str()).collect() }
    old.map_or(!runtime(new).is_empty(), |old| runtime(old) != runtime(new))
}
fn affected_modules(old: Option<&ScopeGeneration>, new: &ScopeGeneration) -> Vec<String> {
    let old: BTreeMap<_, _> = old.into_iter().flat_map(|g| g.targets.signed.packages.values()).map(|p| (p.manifest.id.as_str(), p.digest.as_str())).collect();
    let new: BTreeMap<_, _> = new.targets.signed.packages.values().map(|p| (p.manifest.id.as_str(), p.digest.as_str())).collect();
    old.keys().chain(new.keys()).copied().collect::<BTreeSet<_>>().into_iter().filter(|module| old.get(module) != new.get(module)).map(str::to_owned).collect()
}
fn check_freshness(operation: &UpdateOperation, now: u64) -> Result<()> {
    if now < operation.verified_at || now >= operation.metadata_expires_at {
        Err(UpdateError::MetadataExpired { verified_at: operation.verified_at, expires_at: operation.metadata_expires_at, now })
    } else { Ok(()) }
}
