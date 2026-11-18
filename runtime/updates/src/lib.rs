mod model;
mod network;
mod lifecycle;
mod leases;
mod trust;
mod inventory;
mod closure;
mod migration;
pub use model::*;

use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};
use mintcat_feature_backup::{MaintenanceAdmission, MaintenanceGate};
use mintcat_runtime_artifacts::{ArtifactError, ArtifactStore, GarbageCollectionReport};
use mintcat_runtime_kernel::{KernelDatabase, KernelDatabaseError, RuntimeLayout};
use mintcat_runtime_release::{LocalRepository, RepositoryBundle, RepositoryVerifier, VerificationPolicy, VerifiedScopeRelease};
use mintcat_runtime_supervisor::{ModuleSupervisor, SupervisorError};
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
    prepared: Mutex<HashSet<String>>,
    recovered: AtomicBool,
    startup_readonly: AtomicBool,
    leases_reconciled: AtomicBool,
    lifecycle: tokio::sync::Mutex<()>,
}
impl UpdatesCoordinator {
    pub fn open(layout: RuntimeLayout, database: KernelDatabase, artifacts: ArtifactStore, verifier: RepositoryVerifier, policy: VerificationPolicy, installation_id: String) -> Result<Self> {
        require(layout.initialized && !installation_id.is_empty(), "initialized layout and stable installation identity required")?;
        database.apply_migration("runtime.updates", 1, "CREATE TABLE update_state(id INTEGER PRIMARY KEY CHECK(id=1), state_json TEXT NOT NULL); CREATE TABLE update_events(seq INTEGER PRIMARY KEY AUTOINCREMENT, event TEXT NOT NULL);")?;
        database.connection()?.execute("INSERT OR IGNORE INTO update_state(id,state_json) VALUES(1,?1)", [serde_json::to_string(&UpdateState::default())?])?;
        let coordinator = Self { inner: Arc::new(Inner { layout, database, artifacts, verifier, policy, installation_id, supervisors: Mutex::new(HashMap::new()), prepared: Mutex::new(HashSet::new()), recovered: AtomicBool::new(false), startup_readonly: AtomicBool::new(false), leases_reconciled: AtomicBool::new(false), lifecycle: tokio::sync::Mutex::new(()) }) };
        coordinator.migrate_legacy_state()?;
        Ok(coordinator)
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
    pub fn environment(&self) -> Result<EnvironmentSnapshot> { Ok(self.read_state()?.environment) }
    pub fn operation(&self, operation_id: &str) -> Result<UpdateOperation> { self.read_state()?.operations.remove(operation_id).ok_or_else(|| UpdateError::Blocked("unknown update operation".into())) }
    pub fn operations(&self) -> Result<Vec<UpdateOperation>> { Ok(self.read_state()?.operations.into_values().collect()) }
    pub fn scope_revision(&self, scope: &str) -> Result<u64> { Ok(self.read_state()?.revisions.get(scope).copied().unwrap_or(0)) }

    fn begin(&self, bundle: RepositoryBundle, verified: VerifiedScopeRelease, expected: u64, now: u64) -> Result<UpdateOperation> {
        self.available()?;
        verified.require_fresh(now)?;
        mintcat_runtime_release::validate_game_targets(&verified.targets().signed)?;
        for record in verified.package_records().values() { inventory::signed_game(record)?; }
        let release = verified.release();
        require(verified.cohort().includes(&release.scope_id, &release.channel, &self.inner.installation_id)?, "installation is outside signed release cohort")?;
        let metadata_digest = mintcat_runtime_release::sha256(&serde_json::to_vec(verified.targets())?);
        let key = format!("{}:{}:{}", release.scope_id, release.channel, release.target);
        self.change("metadata-verified", |state| {
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
                previous, phase: UpdatePhase::MetadataVerified, failure: None, verified_at: now, metadata_expires_at: verified.metadata_expires_at(), quarantined: false, bundle, backups: BTreeMap::new(), drained: BTreeSet::new(), migrated: BTreeSet::new() };
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
        self.change("update-rejected", |s| { let op = s.operations.get_mut(operation_id).ok_or_else(|| UpdateError::Blocked("unknown update operation".into()))?; require(!matches!(op.phase, UpdatePhase::Probation | UpdatePhase::Confirmed | UpdatePhase::RolledBack), "published operation must use scoped rollback, not rejection")?; op.quarantined = !matches!(error, UpdateError::MetadataExpired { .. } | UpdateError::Io(_) | UpdateError::Artifact(ArtifactError::Io(_))); op.phase = UpdatePhase::Rejected; op.failure = Some(error.to_string()); Ok(()) })
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
    async fn start_generation(&self, generation: &ScopeGeneration) -> Result<ModuleSupervisor> {
        self.verify_offline(generation)?;
        let environment = EnvironmentSnapshot { generation_id: generation.generation_id.clone(), scopes: BTreeMap::from([(generation.targets.signed.release.scope_id.clone(), generation.clone())]) };
        self.build_closure_supervisor(&environment).await
    }
    pub async fn prepare(&self, operation_id: &str, now: u64) -> Result<UpdateOperation> {
        let started = std::time::Instant::now();
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.available()?;
        let operation = self.operation(operation_id)?;
        require(matches!(operation.phase, UpdatePhase::Staged | UpdatePhase::WaitingForSafePoint), "prepare requires staged verified closure or waiting safe point")?;
        let result: Result<UpdateOperation> = async {
            check_freshness(&operation, now)?;
            self.phase(operation_id, UpdatePhase::WaitingForSafePoint)?;
            let _admission = MaintenanceGate::enter_mutation(&self.inner.layout)?;
            let state = self.read_state()?;
            if let Some(previous) = &operation.previous {
                let digests = previous.digests();
                if state.leases.values().any(|lease| lease.running && matches!(lease.kind, LeaseKind::Task | LeaseKind::Service) && !lease.digests.is_disjoint(&digests)) {
                    return Err(UpdateError::Busy("game remains pinned by an unfinished job; activation waits for its safe point".into()));
                }
            }
            let supervisor = self.start_generation(&operation.generation).await?;
            if let Err(error) = check_freshness(&operation, now.saturating_add(started.elapsed().as_secs())) {
                let _ = supervisor.shutdown_all().await;
                return Err(error);
            }
            self.inner.supervisors.lock().expect("supervisor registry poisoned").insert(operation.generation.generation_id.clone(), supervisor);
            self.inner.prepared.lock().expect("prepared registry poisoned").insert(operation_id.into());
            self.phase(operation_id, UpdatePhase::Prepared)
        }.await;
        if let Err(error) = &result {
            if matches!(error, UpdateError::Busy(_) | UpdateError::Maintenance(mintcat_feature_backup::BackupError::Busy(_))) {
                self.change("waiting-for-safe-point", |s| { let op = s.operations.get_mut(operation_id).expect("operation exists"); op.phase = UpdatePhase::WaitingForSafePoint; op.failure = Some(error.to_string()); Ok(()) })?;
            } else { self.reject(operation_id, error)?; }
        }
        result
    }
    pub fn activate(&self, operation_id: &str, now: u64) -> Result<UpdateOperation> {
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
            require(!operation.generation.digests().iter().any(|d| s.revoked.contains(d)), "candidate revoked before publication")?;
            let revision = s.revisions.get(&operation.scope_id).copied().unwrap_or(0);
            if let Some(previous) = &operation.previous {
                let digests = previous.digests();
                if s.leases.values().any(|lease| lease.running && matches!(lease.kind, LeaseKind::Task | LeaseKind::Service) && !lease.digests.is_disjoint(&digests)) {
                    return Err(UpdateError::Busy("game admitted work after preparation; activation waits for its safe point".into()));
                }
            }
            require(revision == operation.expected_scope_revision, "expectedScopeRevision conflict against latest environment")?;
            s.revisions.insert(operation.scope_id.clone(), operation.generation.revision);
            s.environment.scopes.insert(operation.scope_id.clone(), operation.generation.clone());
            s.environment.generation_id = operation.generation.generation_id.clone();
            if let Some(previous) = &operation.previous {
                let lease = DurableLease { lease_id: format!("rollback:{operation_id}"), owner_id: operation_id.into(), kind: LeaseKind::Rollback, environment: EnvironmentSnapshot { generation_id: previous.generation_id.clone(), scopes: BTreeMap::from([(operation.scope_id.clone(), previous.clone())]) }, digests: previous.digests(), expires_at: None, running: false };
                s.leases.insert(lease.lease_id.clone(), lease);
            }
            let op = s.operations.get_mut(operation_id).expect("operation exists"); op.phase = UpdatePhase::Probation; Ok(op.clone())
        })
    }
    pub async fn confirm(&self, operation_id: &str, now: u64) -> Result<UpdateOperation> {
        let started = std::time::Instant::now();
        let _lifecycle = self.inner.lifecycle.lock().await;
        let operation = self.operation(operation_id)?;
        require(operation.phase == UpdatePhase::Probation, "confirmation requires probation and actual worker health")?;
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
        if let Err(error) = check_freshness(&operation, now.saturating_add(started.elapsed().as_secs())) {
            self.change("metadata-expired-during-readiness", |state| { let operation = state.operations.get_mut(operation_id).expect("operation exists"); operation.failure = Some(error.to_string()); operation.quarantined = false; Ok(()) })?;
            drop(_lifecycle); self.rollback(operation_id, &error.to_string()).await?; return Err(error);
        }
        // Persist confirmed routing before new game jobs may be admitted.
        let confirmed = self.change("confirmed", |s| {
            let generation = s.environment.scopes.get_mut(&operation.scope_id).ok_or_else(|| UpdateError::Blocked("scope missing".into()))?;
            require(generation.generation_id == operation.generation.generation_id, "scope advanced beyond probation")?;
            generation.confirmed = true;
            s.scope_blockers.remove(&operation.scope_id);
            let op = s.operations.get_mut(operation_id).expect("operation exists"); op.generation.confirmed = true; op.phase = UpdatePhase::Confirmed; Ok(op.clone())
        })?;
        Ok(confirmed)
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
fn check_freshness(operation: &UpdateOperation, now: u64) -> Result<()> {
    if now < operation.verified_at || now >= operation.metadata_expires_at {
        Err(UpdateError::MetadataExpired { verified_at: operation.verified_at, expires_at: operation.metadata_expires_at, now })
    } else { Ok(()) }
}
