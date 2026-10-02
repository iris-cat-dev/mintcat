use super::*;

impl UpdatesCoordinator {
    /// Retains an authenticated saved source environment but only the caller's actual
    /// code closure. Incidental Library/game entries in its lock do not pin their writers.
    pub fn retain_environment_lease(&self, owner_id: &str, environment: &EnvironmentSnapshot, digests: &[String]) -> Result<DurableLease> {
        require(!owner_id.is_empty(), "saved environment lease owner required")?;
        self.authenticate_environment(environment)?;
        let declared: BTreeSet<_> = environment.scopes.values().flat_map(ScopeGeneration::digests).collect();
        let requested: BTreeSet<_> = digests.iter().cloned().collect();
        require(requested.is_subset(&declared), "environment lease includes an undeclared digest")?;
        let _admission = if self.inner.recovered.load(Ordering::Acquire) { Some(MaintenanceGate::enter_mutation(&self.inner.layout)?) } else { require(!self.inner.leases_reconciled.load(Ordering::Acquire), "saved environment recovery already finished")?; None };
        let artifacts = self.artifact_store_for_digests(digests)?;
        let _guards = digests.iter().map(|digest| artifacts.acquire_module(digest)).collect::<std::result::Result<Vec<_>, _>>()?;
        let lease_id = format!("environment-lease:{owner_id}");
        self.change("saved-environment-retained", |state| {
            if let Some(existing) = state.leases.get(&lease_id) {
                require(existing.digests == requested && serde_json::to_value(&existing.environment)? == serde_json::to_value(environment)?, "saved environment identity reused with a different immutable closure")?;
                return Ok(existing.clone());
            }
            let lease = DurableLease { lease_id, owner_id: owner_id.into(), kind: LeaseKind::Task, environment: environment.clone(), digests: requested, expires_at: None, running: true };
            state.leases.insert(lease.lease_id.clone(), lease.clone()); Ok(lease)
        })
    }
    /// Pins an immutable job/receipt closure before dispatch. Every digest is loaded through
    /// the signed ArtifactStore and held by live guards until durable lease registration commits.
    pub fn pin_artifacts(&self, owner_id: &str, kind: LeaseKind, digests: &[String], expires_at: Option<u64>, running: bool) -> Result<DurableLease> {
        self.available()?;
        let _admission = MaintenanceGate::enter_mutation(&self.inner.layout)?;
        self.pin_exact(owner_id, kind, digests, expires_at, running, false)
    }
    /// Recovery-only authority input from durable JobStore/receipt owners; revocation cannot
    /// discard a required unfinished commit/rollback closure. This does not admit a new job.
    pub fn restore_artifact_lease(&self, owner_id: &str, kind: LeaseKind, digests: &[String], expires_at: Option<u64>, running: bool) -> Result<DurableLease> {
        require(!self.inner.leases_reconciled.load(Ordering::Acquire), "lease recovery already completed")?;
        self.pin_exact(owner_id, kind, digests, expires_at, running, true)
    }
    fn pin_exact(&self, owner_id: &str, kind: LeaseKind, digests: &[String], expires_at: Option<u64>, running: bool, recovery: bool) -> Result<DurableLease> {
        require(!owner_id.is_empty() && !digests.is_empty() && kind != LeaseKind::Ui, "exact artifact lease requires owner/nonempty closure; UI uses signed generation session")?;
        let guards: Vec<_> = digests.iter().map(|digest| self.artifact_store_for_digest(digest)?.acquire_module(digest).map_err(UpdateError::from)).collect::<Result<_>>()?;
        let digests: BTreeSet<_> = guards.iter().map(|lease| lease.module().digest.clone()).collect();
        let lease_id = format!("artifact-lease:{kind:?}:{owner_id}");
        self.change("exact-artifact-lease-pinned", |s| {
            if !recovery {
                require(!digests.iter().any(|d| s.revoked.contains(d)), "revoked package cannot admit new task/service")?;
                let admitted: BTreeSet<_> = s.environment.scopes.iter().filter(|(scope, g)| g.confirmed && !s.scope_blockers.contains_key(*scope)).flat_map(|(_, g)| g.digests()).collect();
                require(digests.is_subset(&admitted), "new task/service closure must belong to confirmed signed active scopes, not merely an installed/staged package")?;
            }
            if let Some(existing) = s.leases.get(&lease_id) {
                require(existing.kind == kind && existing.owner_id == owner_id && existing.digests == digests, "owner lease identity reused with a different immutable closure")?;
                return Ok(existing.clone());
            }
            let lease = DurableLease { lease_id, owner_id: owner_id.into(), kind, environment: EnvironmentSnapshot { generation_id: format!("closure:{owner_id}"), scopes: BTreeMap::new(), disabled_ui: BTreeSet::new() }, digests, expires_at, running };
            s.leases.insert(lease.lease_id.clone(), lease.clone()); Ok(lease)
        })
    }
    /// Called only after every incomplete job, approved plan, active receipt chain and
    /// broker-connected UI session has been reconstructed from its authoritative owner.
    pub fn finish_lease_reconciliation(&self) -> Result<()> {
        self.available()?;
        self.change("lease-reconciliation-finished", |_| Ok(()))?;
        self.inner.leases_reconciled.store(true, Ordering::Release); Ok(())
    }
    pub fn heartbeat_ui_session(&self, session_id: &str, generation_id: &str, now: u64) -> Result<()> {
        self.change("ui-session-heartbeat", |s| {
            let lease = s.leases.get_mut(&format!("ui:{session_id}")).ok_or_else(|| UpdateError::Blocked("UI session not registered".into()))?;
            require(lease.running && lease.environment.generation_id == generation_id, "UI session generation mismatch/disconnected")?;
            lease.expires_at = Some(now.checked_add(90).ok_or_else(|| UpdateError::Blocked("UI expiry overflow".into()))?); Ok(())
        })
    }
    /// Expiration denies new calls, but does not establish that loaded chunks were released.
    /// The broker must explicitly close a disconnected session to permit artifact deletion.
    pub fn authorize_ui_session(&self, session_id: &str, generation_id: &str, now: u64) -> Result<EnvironmentSnapshot> {
        let state = self.read_state()?;
        let lease = state.leases.get(&format!("ui:{session_id}")).ok_or_else(|| UpdateError::Blocked("UI session not registered".into()))?;
        require(lease.running && lease.environment.generation_id == generation_id && lease.expires_at.is_none_or(|expiry| expiry > now), "UI session expired/disconnected/generation mismatch; reconnect before new calls")?;
        Ok(lease.environment.clone())
    }
    pub fn authorize_channel(&self, scope: &str, channel: &str, expected_scope_revision: u64) -> Result<()> {
        self.available()?;
        self.change("scope-channel-authorized", |s| {
            require(!channel.is_empty() && s.revisions.get(scope).copied().unwrap_or(0) == expected_scope_revision, "channel authorization revision conflict")?;
            s.authorized_channels.insert(scope.into(), channel.into()); Ok(())
        })
    }
}
