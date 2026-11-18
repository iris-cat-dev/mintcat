use super::*;

impl UpdatesCoordinator {
    /// Pins an immutable job/receipt closure before dispatch. Every digest is loaded through
    /// the signed ArtifactStore and held by live guards until durable lease registration commits.
    pub fn pin_artifacts(&self, owner_id: &str, kind: LeaseKind, digests: &[String], expires_at: Option<u64>, running: bool) -> Result<DurableLease> {
        self.available()?;
        let _admission = MaintenanceGate::enter_mutation(&self.inner.layout)?;
        self.pin_exact(owner_id, kind, digests, expires_at, running, false)
    }
    /// A trusted plan/receipt owner may retain its original admitted package identity
    /// for approved apply/uninstall; active code must never replace this saved closure.
    pub fn retain_install_artifacts(&self, owner_id: &str, digests: &[String], running: bool) -> Result<DurableLease> {
        self.available()?;
        let _admission = MaintenanceGate::enter_mutation(&self.inner.layout)?;
        self.pin_exact(owner_id, LeaseKind::Task, digests, None, running, true)
    }
    /// Recovery-only authority input from durable JobStore/receipt owners; revocation cannot
    /// discard a required unfinished commit/rollback closure. This does not admit a new job.
    pub fn restore_artifact_lease(&self, owner_id: &str, kind: LeaseKind, digests: &[String], expires_at: Option<u64>, running: bool) -> Result<DurableLease> {
        require(!self.inner.leases_reconciled.load(Ordering::Acquire), "lease recovery already completed")?;
        self.pin_exact(owner_id, kind, digests, expires_at, running, true)
    }
    fn pin_exact(&self, owner_id: &str, kind: LeaseKind, digests: &[String], expires_at: Option<u64>, running: bool, recovery: bool) -> Result<DurableLease> {
        require(!owner_id.is_empty() && !digests.is_empty(), "exact artifact lease requires owner and nonempty closure")?;
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
                let mut lease = existing.clone();
                lease.running = running;
                s.leases.insert(lease_id.clone(), lease.clone());
                return Ok(lease);
            }
            let lease = DurableLease { lease_id, owner_id: owner_id.into(), kind, environment: EnvironmentSnapshot { generation_id: format!("closure:{owner_id}"), scopes: BTreeMap::new() }, digests, expires_at, running };
            s.leases.insert(lease.lease_id.clone(), lease.clone()); Ok(lease)
        })
    }
    /// Called after incomplete jobs, approved plans and active receipts have been
    /// reconstructed from their authoritative on-disk owners.
    pub fn finish_lease_reconciliation(&self) -> Result<()> {
        self.available()?;
        self.change("lease-reconciliation-finished", |_| Ok(()))?;
        self.inner.leases_reconciled.store(true, Ordering::Release); Ok(())
    }
    pub fn authorize_channel(&self, scope: &str, channel: &str, expected_scope_revision: u64) -> Result<()> {
        self.available()?;
        self.change("scope-channel-authorized", |s| {
            require(!channel.is_empty() && s.revisions.get(scope).copied().unwrap_or(0) == expected_scope_revision, "channel authorization revision conflict")?;
            s.authorized_channels.insert(scope.into(), channel.into()); Ok(())
        })
    }
}
