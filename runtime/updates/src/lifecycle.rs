use super::*;

impl UpdatesCoordinator {
    /// Local rollback is a scoped delta over the newest environment, never an old whole lock.
    pub async fn rollback(&self, operation_id: &str, reason: &str) -> Result<UpdateOperation> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        let _publication = self.publication_guard()?;
        let operation = self.operation(operation_id)?;
        require(!matches!(operation.phase, UpdatePhase::Rejected | UpdatePhase::RolledBack), "operation already terminal")?;
        let initial = self.read_state()?;
        let initial_current = initial.environment.scopes.get(&operation.scope_id);
        require(initial_current.map_or(operation.expected_scope_revision == 0, |generation| generation.generation_id == operation.generation.generation_id || operation.previous.as_ref().is_some_and(|previous| generation.generation_id == previous.generation_id)), "scope has a later update; this operation cannot roll it back")?;
        if let Some(previous) = &operation.previous { require(!previous.digests().iter().any(|digest| initial.revoked.contains(digest)), "rollback cannot activate revoked previous code")?; }
        let mut acquired_gate = false;
        let needs_gate = !operation.drained.is_empty() || !operation.backups.is_empty() || operation.generation.targets.signed.packages.values().any(|p| p.manifest.components.iter().any(|c| c.state_model != "stateless"));
        if needs_gate {
            let owned = self.inner.maintenance.lock().expect("maintenance mutex poisoned").as_ref().map(|(owner, _)| owner == operation_id);
            if owned == Some(false) { return Err(UpdateError::Busy("another update owns maintenance".into())); }
            if owned.is_none() {
                let gate = MaintenanceGate::exclusive(&self.inner.layout, true)?;
                if let Some(marker) = gate.read()? { require(marker.owner_id == "runtime.updates" && marker.operation_id.as_deref() == Some(operation_id), "maintenance marker belongs to a different recovery operation")?; }
                *self.inner.maintenance.lock().expect("maintenance mutex poisoned") = Some((operation_id.into(), gate));
                acquired_gate = true;
            }
        }
        let state = self.read_state()?;
        let current = state.environment.scopes.get(&operation.scope_id).cloned();
        let new_writes = current.as_ref().filter(|g| g.generation_id == operation.generation.generation_id).map_or(operation.generation.new_writes, |g| g.new_writes);
        if new_writes && (!operation.migrated.is_empty() || (operation.previous.is_none() && !operation.backups.is_empty())) {
            self.change("rollback-refused-after-new-write", |s| { s.recovery_required = Some(format!("{}: old database restore prohibited after new writes", operation.scope_id)); s.recovery_operation = Some(operation_id.into()); let op = s.operations.get_mut(operation_id).expect("operation exists"); op.phase = UpdatePhase::RecoveryRequired; op.failure = s.recovery_required.clone(); Ok(()) })?;
            return Err(UpdateError::Blocked("rollback after first-new-write refused: preserve current database; use owner repair/export or explicitly compatible code".into()));
        }
        require(current.as_ref().map_or(operation.expected_scope_revision == 0, |g| g.generation_id == operation.generation.generation_id || operation.previous.as_ref().is_some_and(|p| g.generation_id == p.generation_id)), "scope has a later update; this operation cannot roll it back")?;
        if let Some(previous) = &operation.previous { require(!previous.digests().iter().any(|digest| state.revoked.contains(digest)), "rollback cannot activate revoked previous code")?; }
        if needs_gate {
            let owner_digests: BTreeSet<_> = current.as_ref().into_iter().flat_map(|generation| generation.targets.signed.packages.values()).filter(|record| operation.drained.contains(&record.manifest.id) && record.manifest.components.iter().any(|component| component.state_model != "stateless")).map(|record| record.digest.clone()).collect();
            if state.leases.values().any(|lease| matches!(lease.kind, LeaseKind::Task | LeaseKind::Service) && !lease.digests.is_disjoint(&owner_digests)) {
                if acquired_gate { self.clear_maintenance(operation_id)?; }
                return Err(UpdateError::Busy("state owner remains pinned by an unfinished task or service".into()));
            }
        }
        // Persist the rollback intent before any process stop or data restore. Restart
        // must finish this intent, not reopen the candidate over a partly restored DB.
        self.phase(operation_id, UpdatePhase::RecoveryRequired)?;
        let leased = state.leases.values().any(|lease| lease.running && matches!(lease.kind, LeaseKind::Task | LeaseKind::Service) && !lease.digests.is_disjoint(&operation.generation.digests()));
        let supervisor = if leased { None } else { self.inner.supervisors.lock().expect("supervisor registry poisoned").remove(&operation.generation.generation_id) };
        if let Some(supervisor) = supervisor { supervisor.shutdown_all().await?; }
        let result: Result<UpdateOperation> = async {
            if !operation.drained.is_empty() || !operation.backups.is_empty() || operation.generation.targets.signed.packages.values().any(|p| p.manifest.components.iter().any(|c| c.state_model != "stateless")) {
                for owner_id in &operation.drained {
                    let owner = self.inner.owners.lock().expect("owner registry poisoned").get(owner_id).cloned().ok_or_else(|| UpdateError::Blocked(format!("recovery requires actual state owner {owner_id}")))?;
                    owner.drain().await?;
                    if let Some(previous) = operation.previous.as_ref().filter(|generation| generation.targets.signed.packages.values().any(|record| record.manifest.id == *owner_id && record.manifest.components.iter().any(|component| component.state_model != "stateless"))) {
                        self.verify_offline(previous)?;
                        owner.rollback_code(previous, self.generation_store(previous)?).await?;
                        if operation.migrated.contains(owner_id) { owner.restore(operation.backups.get(owner_id).ok_or_else(|| UpdateError::Blocked("migration has no durable backup".into()))?).await?; }
                        owner.read_only_health().await?;
                        self.inner.native_ready.lock().expect("native owner readiness poisoned").insert((previous.generation_id.clone(), owner_id.clone()));
                    } else {
                        owner.deactivate_code(&operation.generation).await?;
                    }
                }
            }
            if let Some(previous) = &operation.previous {
                let exists = self.inner.supervisors.lock().expect("supervisor registry poisoned").contains_key(&previous.generation_id);
                if !exists || !operation.drained.is_empty() {
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
            if !self.inner.startup_readonly.load(Ordering::Acquire) {
                for owner_id in &operation.drained {
                    if !operation.previous.as_ref().is_some_and(|generation| generation.targets.signed.packages.values().any(|record| record.manifest.id == *owner_id && record.manifest.components.iter().any(|component| component.state_model != "stateless"))) { continue; }
                    let owner = self.inner.owners.lock().expect("owner registry poisoned").get(owner_id).cloned().expect("owner checked"); owner.open_writes().await?;
                }
            }
            self.clear_maintenance(operation_id)?; Ok(rolled_back)
        }.await;
        if let Err(error) = &result { self.change("rollback-recovery-required", |s| {
            if !operation.drained.is_empty() || !operation.backups.is_empty() || operation.scope_id == "platform" { s.recovery_required = Some(error.to_string()); s.recovery_operation = Some(operation_id.into()); }
            else { s.scope_blockers.insert(operation.scope_id.clone(), error.to_string()); }
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
            self.inner.policy.validate(&record.manifest)?;
            let installed = artifacts.load_module(digest)?;
            require(installed.manifest == record.manifest && installed.signature == record.signature, "offline installed package differs from persisted signed record")?;
            root.verify_package_signature(&installed.manifest, &installed.signature, &mintcat_runtime_artifacts::module_manifest_signing_digest(&installed.manifest)?)?;
        }
        Ok(())
    }
    /// Must run before the Application starts any mutation owner. Pending activations are
    /// either locally rolled back with owner recovery or leave the runtime read-only rescue.
    pub async fn recover_startup_readonly(&self) -> Result<()> {
        self.inner.recovered.store(false, Ordering::Release);
        self.inner.startup_readonly.store(true, Ordering::Release);
        // Connect authenticated CURRENT native owner code before any backup/legacy
        // marker recovery. No drain, initialization or mutating owner recovery here.
        self.inner.native_ready.lock().expect("native owner readiness poisoned").clear();
        for (scope, generation) in &self.environment()?.scopes {
            if !generation.confirmed || !generation.targets.signed.packages.values().any(|p| p.manifest.components.iter().any(|c| c.state_model != "stateless")) { continue; }
            let result: Result<()> = async {
                self.verify_offline(generation)?;
                for record in generation.targets.signed.packages.values().filter(|p| p.manifest.components.iter().any(|c| c.state_model != "stateless")) {
                    if self.read_state()?.revoked.contains(&record.digest) { return Err(UpdateError::ArtifactRevoked(record.digest.clone())); }
                    let owner = self.inner.owners.lock().expect("owner registry poisoned").get(&record.manifest.id).cloned().ok_or_else(|| UpdateError::Blocked(format!("confirmed native owner {} missing", record.manifest.id)))?;
                    owner.prepare_code(generation, self.generation_store(generation)?).await?;
                    owner.read_only_health().await?;
                    self.inner.native_ready.lock().expect("native owner readiness poisoned").insert((generation.generation_id.clone(), owner.owner_id().into()));
                }
                Ok(())
            }.await;
            if let Err(error) = result { self.change("readonly-native-prime-failed", |state| { state.scope_blockers.insert(scope.clone(), error.to_string()); Ok(()) })?; }
        }
        let operations = self.operations()?;
        for operation in operations {
            match operation.phase {
                UpdatePhase::Probation | UpdatePhase::Prepared | UpdatePhase::WaitingForSafePoint | UpdatePhase::RecoveryRequired => {
                    if let Err(error) = self.rollback(&operation.operation_id, "startup interrupted activation recovery").await {
                        if !operation.drained.is_empty() || !operation.backups.is_empty() || operation.scope_id == "platform" { return Err(error); }
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
                if let Some(record) = generation.targets.signed.packages.values().find(|record| state.revoked.contains(&record.digest) && record.manifest.components.iter().any(|c| c.kind == "service" && c.state_model != "stateless")) { return Err(UpdateError::ArtifactRevoked(record.digest.clone())); }
                let owners: Vec<_> = generation.targets.signed.packages.values().filter(|p| p.manifest.components.iter().any(|c| c.state_model != "stateless")).map(|p| self.inner.owners.lock().expect("owner registry poisoned").get(&p.manifest.id).cloned().ok_or_else(|| UpdateError::Blocked(format!("confirmed native owner {} missing", p.manifest.id)))).collect::<Result<_>>()?;
                for owner in owners {
                    if !self.inner.native_ready.lock().expect("native owner readiness poisoned").contains(&(generation.generation_id.clone(), owner.owner_id().into())) {
                        owner.prepare_code(generation, self.generation_store(generation)?).await?;
                    }
                    owner.read_only_health().await?;
                    self.inner.native_ready.lock().expect("native owner readiness poisoned").insert((generation.generation_id.clone(), owner.owner_id().into()));
                }
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
                    self.change("confirmed-scope-startup-blocked", |s| { s.scope_blockers.insert(scope.clone(), error.to_string()); if scope == "platform" { s.recovery_required = Some(format!("platform: {error}")); } Ok(()) })?;
                }
            }
        }
        Ok(())
    }
    /// Called only after the actual Backup owner has recovered/cleared its marker.
    /// Unknown foreign maintenance remains a hard read-only rescue boundary.
    pub async fn finish_startup_recovery(&self) -> Result<()> {
        require(self.inner.startup_readonly.load(Ordering::Acquire), "readonly startup verification must precede write opening")?;
        // A confirmed migration may have crashed between durable confirmation and opening writes.
        let gate = MaintenanceGate::exclusive(&self.inner.layout, true)?;
        if let Some(marker) = gate.read()? {
            require(marker.owner_id == "runtime.updates", "another owner has unresolved maintenance; startup remains read-only")?;
            let operation_id = marker.operation_id.as_deref().ok_or_else(|| UpdateError::Blocked("update maintenance marker lacks operation identity".into()))?;
            let operation = self.operation(operation_id)?;
            require(operation.phase == UpdatePhase::Confirmed, "migration marker requires explicit recovery")?;
            for (owner, _, _) in self.owners_for(&operation)? {
                if operation.generation.targets.signed.packages.values().any(|record| record.manifest.id == owner.owner_id() && record.manifest.components.iter().any(|component| component.state_model != "stateless")) { owner.read_only_health().await?; }
            }
            gate.clear()?;
        }
        drop(gate);
        let current = self.read_state()?;
        require(current.recovery_required.is_none(), "unresolved runtime rescue state")?;
        for (scope, generation) in &current.environment.scopes {
            if current.scope_blockers.contains_key(scope) { continue; }
            for record in generation.targets.signed.packages.values().filter(|p| p.manifest.components.iter().any(|c| c.state_model != "stateless")) {
                let owner = self.inner.owners.lock().expect("owner registry poisoned").get(&record.manifest.id).cloned().ok_or_else(|| UpdateError::Blocked("confirmed state owner missing at write opening".into()))?;
                owner.open_writes().await?;
            }
        }
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
                EnvironmentSnapshot { generation_id: generation.generation_id.clone(), scopes: BTreeMap::from([(scope.into(), generation.clone())]), disabled_ui: s.environment.disabled_ui.clone() }
            } else { require(s.environment.generation_id == expected_generation_id, "environment generation conflict")?; let mut environment = s.environment.clone(); environment.scopes.retain(|scope, g| !s.scope_blockers.contains_key(scope) && !g.digests().iter().any(|d| s.revoked.contains(d))); environment };
            let digests: BTreeSet<_> = environment.scopes.values().flat_map(ScopeGeneration::digests).collect();
            require(!digests.iter().any(|d| s.revoked.contains(d)), "revoked package denies new admission")?;
            if kind != LeaseKind::Ui { require(environment.scopes.values().all(|g| g.confirmed), "probation blocks task/service admissions")?; }
            let lease = DurableLease { lease_id: if kind == LeaseKind::Ui { format!("ui:{owner_id}") } else { id("lease") }, owner_id: owner_id.into(), kind, environment, digests, expires_at, running: true };
            require(!s.leases.contains_key(&lease.lease_id), "lease owner already registered")?;
            if kind == LeaseKind::Ui {
                for operation in s.operations.values_mut().filter(|op| op.phase == UpdatePhase::Prepared) {
                    if let Some(barrier) = &mut operation.barrier { barrier.participants.entry(owner_id.into()).or_insert(None); }
                }
            }
            s.leases.insert(lease.lease_id.clone(), lease.clone()); Ok(lease)
        })
    }
    pub fn release_lease(&self, lease_id: &str) -> Result<()> { self.change("lease-released", |s| { require(s.leases.remove(lease_id).is_some(), "unknown durable lease") }) }
    pub fn renew_lease(&self, lease_id: &str, expires_at: u64) -> Result<()> { self.change("lease-renewed", |s| { let lease = s.leases.get_mut(lease_id).ok_or_else(|| UpdateError::Blocked("unknown lease".into()))?; lease.expires_at = Some(expires_at); Ok(()) }) }
    pub fn leases(&self) -> Result<Vec<DurableLease>> { Ok(self.read_state()?.leases.into_values().collect()) }
    pub fn register_ui_session(&self, session_id: &str, generation_id: &str) -> Result<DurableLease> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| UpdateError::Blocked(e.to_string()))?.as_secs();
        self.acquire_lease(None, generation_id, session_id, LeaseKind::Ui, Some(now.checked_add(90).ok_or_else(|| UpdateError::Blocked("UI expiry overflow".into()))?))
    }
    pub fn close_ui_session(&self, session_id: &str) -> Result<()> { self.release_lease(&format!("ui:{session_id}")) }
    pub fn ui_barriers(&self, session_id: &str) -> Result<Vec<UiBarrier>> {
        // A terminal acknowledgement completes this client's work, not the
        // transaction: other clients and the probation gate still must finish.
        Ok(self.operations()?.into_iter().filter(|op| matches!(op.phase, UpdatePhase::Prepared | UpdatePhase::Probation)).filter_map(|op| op.barrier).filter(|b| b.participants.get(session_id).is_some_and(|ack| ack.as_ref().is_none_or(|ack| !matches!(ack.stage, UiStage::Ready | UiStage::Failed)))).collect())
    }
    pub async fn acknowledge_ui(&self, ack: UiAck) -> Result<()> {
        let failed = ack.stage == UiStage::Failed;
        let operation_id = self.change("ui-barrier-ack", |s| {
            let op = s.operations.values_mut().find(|op| op.barrier.as_ref().is_some_and(|b| b.barrier_id == ack.barrier_id)).ok_or_else(|| UpdateError::Blocked("unknown UI barrier".into()))?;
            require(matches!(op.phase, UpdatePhase::Prepared | UpdatePhase::Probation), "UI barrier is no longer pending")?;
            let barrier = op.barrier.as_mut().expect("barrier exists");
            require(barrier.participants.contains_key(&ack.session_id), "session does not participate in this barrier")?;
            if ack.stage == UiStage::Disposed {
                require(ack.state.is_some() || barrier.participants[&ack.session_id].as_ref().is_some_and(|previous| previous.state.is_some()), "UI dispose acknowledgement requires its migratable state snapshot")?;
            }
            if ack.stage == UiStage::Ready {
                require(op.phase == UpdatePhase::Probation, "UI ready requires published probation generation")?;
                require(!barrier.runtime_changed || ack.rebuilt, "shared UI runtime changed: explicit new WebView realm required")?;
                require(barrier.participants[&ack.session_id].as_ref().is_some_and(|a| matches!(a.stage, UiStage::Disposed | UiStage::RebuildRequired)), "UI ready requires prior snapshot/dispose barrier")?;
            }
            let mut durable = ack.clone();
            if durable.state.is_none() { durable.state = barrier.participants[&ack.session_id].as_ref().and_then(|previous| previous.state.clone()); }
            barrier.participants.insert(ack.session_id.clone(), Some(durable)); Ok(op.operation_id.clone())
        })?;
        if failed { self.rollback(&operation_id, ack.error.as_deref().unwrap_or("UI mounting failed")).await?; }
        Ok(())
    }
    /// Targets-role signed revocations use the shared Ed25519 engine and durable anti-replay
    /// high-water. Admission closes immediately; issued task/service leases remain valid.
    pub fn apply_revocations(&self, envelope: &mintcat_runtime_release::Signed<RevocationMetadata>, now: u64) -> Result<()> {
        let _publication = self.publication_guard()?;
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
    pub fn scope_blockers(&self) -> Result<BTreeMap<String, String>> { Ok(self.read_state()?.scope_blockers) }
    pub fn recovery_reason(&self) -> Result<Option<String>> { Ok(self.read_state()?.recovery_required) }
    pub fn module_inventory(&self) -> Result<ModuleInventory> { self.inventory(&self.environment()?, false) }
    pub fn module_inventory_for_generation(&self, generation_id: &str) -> Result<ModuleInventory> {
        let state = self.read_state()?;
        if state.environment.generation_id == generation_id { return self.inventory(&state.environment, false); }
        if let Some(lease) = state.leases.values().find(|l| l.environment.generation_id == generation_id) { return self.inventory(&lease.environment, true); }
        Err(UpdateError::Blocked("generation is not active probation/confirmed or pinned by a durable lease".into()))
    }
    fn inventory(&self, environment: &EnvironmentSnapshot, leased: bool) -> Result<ModuleInventory> {
        let state = self.read_state()?;
        let disabled = &environment.disabled_ui;
        let mut modules = BTreeMap::new();
        for generation in environment.scopes.values() {
            if !leased && (state.scope_blockers.contains_key(&generation.targets.signed.release.scope_id) || generation.digests().iter().any(|d| state.revoked.contains(d))) { continue; }
            self.verify_offline(generation)?;
            for record in generation.targets.signed.packages.values() {
                let key = (record.manifest.id.clone(), record.digest.clone());
                if modules.contains_key(&key) { continue; }
                if record.manifest.files.iter().any(|f| f.path == "ui/contributions.json") {
                    require(!modules.values().any(|entry: &ModuleInventoryEntry| entry.module_id == record.manifest.id), "multiple active UI digests for one module require separate scoped views")?;
                }
                let game = inventory::signed_game(record)?;
                modules.insert(key, ModuleInventoryEntry { module_id: record.manifest.id.clone(), digest: record.digest.clone(), ui_enabled: !disabled.contains(&record.manifest.id), game });
            }
        }
        Ok(ModuleInventory { generation_id: environment.generation_id.clone(), modules: modules.into_values().collect() })
    }
    pub fn set_ui_enabled(&self, module_id: &str, enabled: bool, expected_generation_id: &str) -> Result<ModuleInventory> {
        let _publication = self.publication_guard()?;
        self.set_ui_enabled_inner(module_id, enabled, expected_generation_id)
    }
    pub(super) fn set_ui_enabled_inner(&self, module_id: &str, enabled: bool, expected_generation_id: &str) -> Result<ModuleInventory> {
        self.available()?;
        self.change("ui-enabled-changed", |s| {
            require(s.environment.generation_id == expected_generation_id, "expectedGenerationId conflict")?;
            require(s.environment.scopes.values().any(|g| g.targets.signed.packages.values().any(|p| p.manifest.id == module_id)), "module absent from signed active closure")?;
            if enabled { s.environment.disabled_ui.remove(module_id); } else { s.environment.disabled_ui.insert(module_id.into()); }
            // UI registry generation changes, not backend scope generations or task closures.
            s.environment.generation_id = id("generation"); Ok(())
        })?; self.module_inventory()
    }
    pub async fn collect_garbage(&self, now: u64, external_retained: &HashSet<String>) -> Result<GarbageCollectionReport> {
        self.available()?;
        require(self.inner.leases_reconciled.load(Ordering::Acquire), "GC waits for durable incomplete jobs, receipts and live UI lease reconciliation")?;
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
            for lease in s.leases.values() { retained.extend(lease.digests.iter().cloned()); }
            for op in s.operations.values().filter(|op| !matches!(op.phase, UpdatePhase::Confirmed | UpdatePhase::Rejected | UpdatePhase::RolledBack)) {
                retained.extend(op.generation.digests());
                if let Some(previous) = &op.previous { retained.extend(previous.digests()); }
            }
            Ok(self.inner.artifacts.collect_garbage(&retained)?)
        })
    }
}

impl UpdatesCoordinator {
    /// Called by the actual connection/process owner after proven disconnect, never by a
    /// lease timer. Normal close during WebView rebuild keeps the original barrier token.
    pub fn ui_disconnected(&self, session_id: &str, reason: &str) -> Result<()> {
        require(!reason.is_empty(), "UI disconnect requires the broker's lifecycle reason")?;
        self.change("ui-session-disconnected", |s| {
            s.leases.remove(&format!("ui:{session_id}"));
            for operation in s.operations.values_mut().filter(|op| matches!(op.phase, UpdatePhase::Prepared | UpdatePhase::Probation)) {
                if let Some(barrier) = &mut operation.barrier { barrier.participants.remove(session_id); }
            }
            Ok(())
        })
    }
    /// First headless-confirmed GUI mounting failure uses the same data-write rollback fence.
    /// It never opens a hidden window and never restores an old owner snapshot after new writes.
    pub async fn report_ui_failure(&self, scope: &str, scope_generation_id: &str, reason: &str) -> Result<UpdateOperation> {
        let state = self.read_state()?;
        require(state.environment.scopes.get(scope).is_some_and(|g| g.generation_id == scope_generation_id), "UI failure refers to a stale/unrelated scope generation")?;
        let operation = state.operations.values().find(|op| op.scope_id == scope && op.generation.generation_id == scope_generation_id).ok_or_else(|| UpdateError::Blocked("scope generation has no activation operation".into()))?;
        self.rollback(&operation.operation_id, reason).await
    }
}
