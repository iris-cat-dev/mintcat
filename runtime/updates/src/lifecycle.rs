use super::*;

impl UpdatesCoordinator {
    /// Local rollback is a scoped delta over the newest environment, never an old whole lock.
    pub async fn rollback(&self, operation_id: &str, reason: &str) -> Result<UpdateOperation> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        let operation = self.operation(operation_id)?;
        require(!matches!(operation.phase, UpdatePhase::Rejected | UpdatePhase::RolledBack), "operation already terminal")?;
        let initial = self.read_state()?;
        let initial_current = initial.environment.scopes.get(&operation.scope_id);
        require(initial_current.map_or(operation.expected_scope_revision == 0, |generation| generation.generation_id == operation.generation.generation_id || operation.previous.as_ref().is_some_and(|previous| generation.generation_id == previous.generation_id)), "scope has a later update; this operation cannot roll it back")?;
        if let Some(previous) = &operation.previous { require(!previous.digests().iter().any(|digest| initial.revoked.contains(digest)), "rollback cannot activate revoked previous code")?; }
        require(operation.backups.is_empty() && operation.drained.is_empty() && operation.migrated.is_empty(), "historical owner migration requires the original application's data recovery; game rollback never restores builtin schemas")?;
        let state = self.read_state()?;
        let current = state.environment.scopes.get(&operation.scope_id).cloned();
        require(current.as_ref().map_or(operation.expected_scope_revision == 0, |g| g.generation_id == operation.generation.generation_id || operation.previous.as_ref().is_some_and(|p| g.generation_id == p.generation_id)), "scope has a later update; this operation cannot roll it back")?;
        // Persist the rollback intent before any process stop or data restore. Restart
        // must finish this intent, not reopen the candidate over a partly restored DB.
        self.phase(operation_id, UpdatePhase::RecoveryRequired)?;
        let leased = state.leases.values().any(|lease| lease.running && matches!(lease.kind, LeaseKind::Task | LeaseKind::Service) && !lease.digests.is_disjoint(&operation.generation.digests()));
        let supervisor = if leased { None } else { self.inner.supervisors.lock().expect("supervisor registry poisoned").remove(&operation.generation.generation_id) };
        if let Some(supervisor) = supervisor { supervisor.shutdown_all().await?; }
        let result: Result<UpdateOperation> = async {
            if let Some(previous) = &operation.previous {
                let exists = self.inner.supervisors.lock().expect("supervisor registry poisoned").contains_key(&previous.generation_id);
                if !exists {
                    self.verify_offline(previous)?;
                    let supervisor = self.start_generation(previous).await?;
                    self.inner.supervisors.lock().expect("supervisor registry poisoned").insert(previous.generation_id.clone(), supervisor);
                }
            }
            let rolled_back = self.change("scope-rolled-back", |s| {
                // Recheck inside the same durable transaction after drain/restore/ready.
                let current = s.environment.scopes.get(&operation.scope_id);
                if current.is_some_and(|g| g.generation_id == operation.generation.generation_id) {
                    let revision = current.expect("checked scope").revision.checked_add(1).ok_or_else(|| UpdateError::Blocked("scope revision overflow".into()))?;
                    s.revisions.insert(operation.scope_id.clone(), revision);
                    if let Some(mut previous) = operation.previous.clone() { previous.revision = revision; s.environment.scopes.insert(operation.scope_id.clone(), previous); } else { s.environment.scopes.remove(&operation.scope_id); }
                    s.environment.generation_id = id("generation");
                } else {
                    require(current.map_or(operation.expected_scope_revision == 0, |g| operation.previous.as_ref().is_some_and(|p| p.generation_id == g.generation_id)), "scope advanced while rollback was preparing")?;
                }
                let op = s.operations.get_mut(operation_id).expect("operation exists"); op.quarantined |= op.failure.is_none(); op.phase = UpdatePhase::RolledBack; op.failure = Some(reason.into());
                let result = op.clone();
                s.leases.remove(&format!("rollback:{operation_id}"));
                s.scope_blockers.remove(&operation.scope_id);
                if s.recovery_operation.as_deref() == Some(operation_id) { s.recovery_required = None; s.recovery_operation = None; }
                Ok(result)
            })?;
            Ok(rolled_back)
        }.await;
        if let Err(error) = &result { self.change("rollback-recovery-required", |s| {
            s.scope_blockers.insert(operation.scope_id.clone(), error.to_string());
            let op = s.operations.get_mut(operation_id).expect("operation exists"); op.phase = UpdatePhase::RecoveryRequired; op.failure = Some(error.to_string()); Ok(())
        })?; }
        result
    }
    pub(super) fn verify_offline(&self, generation: &ScopeGeneration) -> Result<()> {
        // Confirmed startup intentionally does not require fresh network timestamp/expiry.
        // Signature, the exact authenticated committed root, and every installed byte apply.
        let root = mintcat_runtime_release::trust_pinned_root(generation.verified_root.clone(), 0)?;
        let artifacts = self.generation_store(generation)?;
        root.verify("targets", &generation.targets)?;
        for (digest, record) in &generation.targets.signed.packages {
            require(record.manifest.target == self.inner.policy.target && record.manifest.host_api == self.inner.policy.host_api, "historical package must match this host target and framed API")?;
            let installed = artifacts.load_module(digest)?;
            require(installed.manifest == record.manifest && installed.signature == record.signature, "offline installed package differs from persisted signed record")?;
            root.verify_package_signature(&installed.manifest, &installed.signature, &mintcat_runtime_artifacts::module_manifest_signing_digest(&installed.manifest)?)?;
        }
        Ok(())
    }
    /// Pending game activation rolls back using exact committed packages. Builtin
    /// databases are outside game release ownership and are never migrated here.
    pub async fn recover_startup_readonly(&self) -> Result<()> {
        self.inner.recovered.store(false, Ordering::Release);
        self.inner.startup_readonly.store(true, Ordering::Release);
        require(self.read_state()?.recovery_required.is_none(), "unfinished historical owner migration requires original-application recovery")?;
        let operations = self.operations()?;
        for operation in operations {
            match operation.phase {
                UpdatePhase::Probation | UpdatePhase::Prepared | UpdatePhase::WaitingForSafePoint | UpdatePhase::RecoveryRequired => {
                    if let Err(error) = self.rollback(&operation.operation_id, "startup interrupted activation recovery").await {
                        if !operation.drained.is_empty() || !operation.backups.is_empty() { return Err(error); }
                        self.change("optional-scope-recovery-blocked", |s| { s.scope_blockers.insert(operation.scope_id.clone(), error.to_string()); Ok(()) })?;
                    }
                }
                UpdatePhase::MetadataVerified | UpdatePhase::Resolved | UpdatePhase::Downloading | UpdatePhase::Staged => {
                    self.change("interrupted-stage-rejected", |s| { let op = s.operations.get_mut(&operation.operation_id).expect("operation exists"); op.phase = UpdatePhase::Rejected; op.failure = Some("interrupted before publication; confirmed closure unchanged".into()); Ok(()) })?;
                }
                _ => {}
            }
        }
        let environment = self.environment()?;
        for (scope, generation) in &environment.scopes {
            require(generation.confirmed, format!("scope {scope} has no confirmed recovery closure"))?;
            let result: Result<()> = async {
                self.verify_offline(generation)?;
                let state = self.read_state()?;
                // Never relaunch withdrawn code on restart. In the live runtime its
                // already-running, needed consumers retain their immutable realm.
                if generation.digests().iter().any(|digest| state.revoked.contains(digest)) { return Ok(()); }
                let supervisor = self.start_generation(generation).await?;
                self.inner.supervisors.lock().expect("supervisor registry poisoned").insert(generation.generation_id.clone(), supervisor);
                self.change("confirmed-scope-ready", |state| { state.scope_blockers.remove(scope); Ok(()) })?;
                Ok(())
            }.await;
            if let Err(error) = result {
                let active_operation = self.operations()?.into_iter().find(|op| op.generation.generation_id == generation.generation_id);
                let restored = if let Some(op) = active_operation { self.rollback(&op.operation_id, &format!("offline confirmed startup: {error}")).await.is_ok() } else { false };
                if !restored {
                    self.change("confirmed-game-startup-blocked", |s| { s.scope_blockers.insert(scope.clone(), error.to_string()); Ok(()) })?;
                }
            }
        }
        Ok(())
    }
    /// Backup marker recovery precedes this opening of game admissions.
    pub async fn finish_startup_recovery(&self) -> Result<()> {
        require(self.inner.startup_readonly.load(Ordering::Acquire), "readonly startup verification must precede admissions")?;
        let _admission = MaintenanceGate::enter_mutation(&self.inner.layout)?;
        require(self.read_state()?.recovery_required.is_none(), "unresolved historical runtime recovery state")?;
        self.inner.startup_readonly.store(false, Ordering::Release);
        self.inner.recovered.store(true, Ordering::Release); Ok(())
    }
    pub async fn recover_startup(&self) -> Result<()> {
        self.recover_startup_readonly().await?;
        self.finish_startup_recovery().await
    }
    pub fn admit_mutation(&self, scope: &str, generation_id: &str) -> Result<MaintenanceAdmission> {
        self.available()?;
        let admission = MaintenanceGate::enter_mutation(&self.inner.layout)?;
        self.change("first-new-write-boundary", |s| {
            require(!s.scope_blockers.contains_key(scope), "scope is blocked pending verified repair; unrelated scopes remain available")?;
            let generation = s.environment.scopes.get_mut(scope).ok_or_else(|| UpdateError::Blocked("unknown scope".into()))?;
            require(generation.generation_id == generation_id && generation.confirmed, "mutation generation mismatch or probation write barrier")?;
            require(!generation.digests().iter().any(|d| s.revoked.contains(d)), "revoked package: new mutation denied; running leases may finish safely")?;
            // Conservative before-dispatch boundary: never infer that a failed callback made no writes.
            generation.new_writes = true;
            for operation in s.operations.values_mut().filter(|op| op.generation.generation_id == generation_id) { operation.generation.new_writes = true; }
            Ok(())
        })?; Ok(admission)
    }
    pub fn acquire_lease(&self, scope: Option<&str>, expected_generation_id: &str, owner_id: &str, kind: LeaseKind, expires_at: Option<u64>) -> Result<DurableLease> {
        self.available()?;
        let _admission = if matches!(kind, LeaseKind::Task | LeaseKind::Service) { Some(MaintenanceGate::enter_mutation(&self.inner.layout)?) } else { None };
        self.change("lease-acquired", |s| {
            let environment = if let Some(scope) = scope {
                require(!s.scope_blockers.contains_key(scope), "scope is blocked pending repair")?;
                let generation = s.environment.scopes.get(scope).ok_or_else(|| UpdateError::Blocked("unknown lease scope".into()))?;
                require(generation.generation_id == expected_generation_id && generation.confirmed, "new task/service requires confirmed exact scope generation")?;
                EnvironmentSnapshot { generation_id: generation.generation_id.clone(), scopes: BTreeMap::from([(scope.into(), generation.clone())]) }
            } else { require(s.environment.generation_id == expected_generation_id, "environment generation conflict")?; let mut environment = s.environment.clone(); environment.scopes.retain(|scope, g| !s.scope_blockers.contains_key(scope) && !g.digests().iter().any(|d| s.revoked.contains(d))); environment };
            let digests: BTreeSet<_> = environment.scopes.values().flat_map(ScopeGeneration::digests).collect();
            require(!digests.iter().any(|d| s.revoked.contains(d)), "revoked package denies new admission")?;
            require(environment.scopes.values().all(|g| g.confirmed), "probation blocks task/service admissions")?;
            let lease = DurableLease { lease_id: id("lease"), owner_id: owner_id.into(), kind, environment, digests, expires_at, running: true };
            require(!s.leases.contains_key(&lease.lease_id), "lease owner already registered")?;
            s.leases.insert(lease.lease_id.clone(), lease.clone()); Ok(lease)
        })
    }
    pub fn release_lease(&self, lease_id: &str) -> Result<()> { self.change("lease-released", |s| { require(s.leases.remove(lease_id).is_some(), "unknown durable lease") }) }
    pub fn renew_lease(&self, lease_id: &str, expires_at: u64) -> Result<()> { self.change("lease-renewed", |s| { let lease = s.leases.get_mut(lease_id).ok_or_else(|| UpdateError::Blocked("unknown lease".into()))?; lease.expires_at = Some(expires_at); Ok(()) }) }
    pub fn leases(&self) -> Result<Vec<DurableLease>> { Ok(self.read_state()?.leases.into_values().collect()) }
    /// Targets-role signed revocations use the shared Ed25519 engine and durable anti-replay
    /// high-water. Admission closes immediately; issued task/service leases remain valid.
    pub fn apply_revocations(&self, envelope: &mintcat_runtime_release::Signed<RevocationMetadata>, now: u64) -> Result<()> {
        self.apply_revocations_inner(envelope, now)
    }
    pub(super) fn apply_revocations_inner(&self, envelope: &mintcat_runtime_release::Signed<RevocationMetadata>, now: u64) -> Result<()> {
        envelope.signed.header.validate("revocations", now)?;
        self.inner.verifier.trusted_root(now)?.verify("targets", envelope)?;
        require(!envelope.signed.reason.is_empty(), "signed revocations require a reason")?;
        for digest in &envelope.signed.digests { require(digest.len() == 71 && digest.starts_with("sha256:") && digest[7..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "invalid signed revoked digest")?; }
        let digest = mintcat_runtime_release::sha256(&serde_json::to_vec(envelope)?);
        self.change("signed-artifacts-revoked", |s| {
            if let Some((version, previous, previous_time)) = &s.revocation_high_water {
                require(now >= *previous_time && envelope.signed.header.version >= *version && (envelope.signed.header.version != *version || digest == *previous), "revocation clock/version rollback/equivocation")?;
                if envelope.signed.header.version == *version { return Ok(()); }
            }
            s.revocation_high_water = Some((envelope.signed.header.version, digest, now));
            s.revoked.extend(envelope.signed.digests.iter().cloned());
            s.environment.generation_id = id("generation"); Ok(())
        })
    }
    /// Reads committed recovery diagnostics even while startup admissions are fenced.
    pub fn rescue_status(&self) -> Result<(Option<String>, BTreeMap<String, String>)> {
        let state = self.read_state()?;
        Ok((state.recovery_required, state.scope_blockers))
    }
    pub fn module_inventory(&self) -> Result<ModuleInventory> { self.inventory(&self.environment()?, false) }
    pub fn module_inventory_for_generation(&self, generation_id: &str) -> Result<ModuleInventory> {
        let state = self.read_state()?;
        if state.environment.generation_id == generation_id { return self.inventory(&state.environment, false); }
        if let Some(lease) = state.leases.values().find(|l| l.environment.generation_id == generation_id) { return self.inventory(&lease.environment, true); }
        Err(UpdateError::Blocked("generation is not active probation/confirmed or pinned by a durable lease".into()))
    }
    fn inventory(&self, environment: &EnvironmentSnapshot, leased: bool) -> Result<ModuleInventory> {
        let state = self.read_state()?;
        let mut modules = BTreeMap::new();
        for (scope, generation) in &environment.scopes {
            if !scope.starts_with("game:") || (!leased && (!generation.confirmed || state.scope_blockers.contains_key(scope) || generation.digests().iter().any(|d| state.revoked.contains(d)))) { continue; }
            self.verify_offline(generation)?;
            for record in generation.targets.signed.packages.values() {
                let key = (record.manifest.id.clone(), record.digest.clone());
                if modules.contains_key(&key) { continue; }
                let Some(game) = inventory::signed_game(record)? else { continue; };
                modules.insert(key, ModuleInventoryEntry { module_id: record.manifest.id.clone(), digest: record.digest.clone(), enabled: true, game: Some(game) });
            }
        }
        Ok(ModuleInventory { generation_id: environment.generation_id.clone(), modules: modules.into_values().collect() })
    }
    pub async fn collect_garbage(&self, now: u64, external_retained: &HashSet<String>) -> Result<GarbageCollectionReport> {
        self.available()?;
        require(self.inner.leases_reconciled.load(Ordering::Acquire), "GC waits for durable incomplete jobs and receipts reconciliation")?;
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.change("expired-idle-leases-collected", |s| { s.leases.retain(|_, l| l.running || l.expires_at.is_none_or(|expiry| expiry > now)); Ok(()) })?;
        let state = self.read_state()?;
        let mut live_generations = HashSet::new();
        for generation in state.environment.scopes.values() { live_generations.insert(generation.generation_id.clone()); }
        let retained: BTreeSet<_> = state.leases.values().filter(|lease| matches!(lease.kind, LeaseKind::Task | LeaseKind::Service)).flat_map(|lease| lease.digests.iter().cloned()).collect();
        for generation in state.operations.values().flat_map(|operation| std::iter::once(&operation.generation).chain(operation.previous.iter())) {
            if generation.digests().iter().any(|digest| retained.contains(digest)) { live_generations.insert(generation.generation_id.clone()); }
        }
        for op in state.operations.values().filter(|op| !matches!(op.phase, UpdatePhase::Confirmed | UpdatePhase::Rejected | UpdatePhase::RolledBack)) { live_generations.insert(op.generation.generation_id.clone()); if let Some(previous) = &op.previous { live_generations.insert(previous.generation_id.clone()); } }
        let stale: Vec<_> = { let mut supervisors = self.inner.supervisors.lock().expect("supervisor registry poisoned"); let stale: Vec<_> = supervisors.keys().filter(|g| !live_generations.contains(*g)).cloned().collect(); stale.into_iter().filter_map(|g| supervisors.remove(&g)).collect() };
        for supervisor in stale { supervisor.shutdown_all().await?; }
        // Recompute under the durable state writer lock so a newly admitted scope/lease cannot
        // race the deletion pass. ArtifactStore also rechecks live process grants.
        self.change("artifact-gc", |s| {
            let mut retained = external_retained.clone();
            for generation in s.environment.scopes.values() { retained.extend(generation.digests()); }
            for generation in s.archived_scopes.values() { retained.extend(generation.digests()); }
            for lease in s.leases.values() { retained.extend(lease.digests.iter().cloned()); }
            for op in s.operations.values().filter(|op| !matches!(op.phase, UpdatePhase::Confirmed | UpdatePhase::Rejected | UpdatePhase::RolledBack)) {
                retained.extend(op.generation.digests());
                if let Some(previous) = &op.previous { retained.extend(previous.digests()); }
            }
            Ok(self.inner.artifacts.collect_garbage(&retained)?)
        })
    }
}

