use super::*;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiArtifactAuthority { pub generation_id: String, pub modules: Vec<UiModuleAuthority> }
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiModuleAuthority {
    pub module_id: String,
    pub digest: String,
    pub package: mintcat_runtime_release::PackageRecord,
    pub verified_root: mintcat_runtime_release::Signed<mintcat_runtime_release::RootMetadata>,
}
enum UiFence { Read { _guard: OwnedRwLockReadGuard<()> }, Write { _guard: OwnedRwLockWriteGuard<()> } }
/// Keep this guard through dispatch acceptance, including a blocking acceptance callback.
/// It intentionally does NOT own MaintenanceAdmission: backup acquires its own exclusive gate.
pub struct UiCallAdmission {
    pub environment: EnvironmentSnapshot,
    pub scope_id: String,
    pub scope_generation_id: String,
    pub package: mintcat_runtime_release::PackageRecord,
    owner: Arc<Inner>,
    _fence: UiFence,
}
impl UpdatesCoordinator {
    pub fn ui_artifact_authority(&self, expected_generation_id: &str) -> Result<UiArtifactAuthority> {
        let state = self.read_state()?;
        require(state.environment.generation_id == expected_generation_id, "expectedGenerationId conflict")?;
        let mut modules = BTreeMap::new();
        for (scope, generation) in &state.environment.scopes {
            if state.scope_blockers.contains_key(scope) { continue; }
            for record in generation.targets.signed.packages.values() {
                if state.environment.disabled_ui.contains(&record.manifest.id) || state.revoked.contains(&record.digest) || !record.manifest.files.iter().any(|f| f.path == "ui/contributions.json") { continue; }
                if let Some(existing) = modules.get(&record.manifest.id) {
                    let existing: &UiModuleAuthority = existing;
                    require(existing.digest == record.digest, "multiple active UI digests for one module")?;
                    continue;
                }
                modules.insert(record.manifest.id.clone(), UiModuleAuthority { module_id: record.manifest.id.clone(), digest: record.digest.clone(), package: record.clone(), verified_root: generation.verified_root.clone() });
            }
        }
        Ok(UiArtifactAuthority { generation_id: state.environment.generation_id, modules: modules.into_values().collect() })
    }
    pub async fn admit_ui_call(&self, session_id: &str, generation_id: &str, module_id: &str, digest: &str, now: u64, environment_change: bool) -> Result<UiCallAdmission> {
        let fence = if environment_change { UiFence::Write { _guard: self.inner.ui_fence.clone().write_owned().await } } else { UiFence::Read { _guard: self.inner.ui_fence.clone().read_owned().await } };
        let state = self.read_state()?;
        let stale = |reason: &str| UpdateError::StaleUiSession(reason.into());
        if state.environment.generation_id != generation_id { return Err(stale("business generation is not current")); }
        let lease = state.leases.get(&format!("ui:{session_id}")).ok_or_else(|| stale("session disconnected"))?;
        if !lease.running || lease.environment.generation_id != generation_id || lease.expires_at.is_none_or(|expiry| expiry <= now) { return Err(stale("session expired or generation mismatch")); }
        if state.revoked.contains(digest) { return Err(UpdateError::ArtifactRevoked(digest.into())); }
        if state.environment.disabled_ui.contains(module_id) { return Err(stale("module UI disabled")); }
        let (scope, generation, record) = state.environment.scopes.iter().find_map(|(scope, generation)| generation.targets.signed.packages.get(digest).filter(|p| p.manifest.id == module_id && p.manifest.files.iter().any(|f| f.path == "ui/contributions.json")).map(|p| (scope, generation, p))).ok_or_else(|| stale("module binding mismatch"))?;
        if state.scope_blockers.contains_key(scope) { return Err(UpdateError::Blocked("UI scope requires recovery".into())); }
        if !environment_change && state.operations.values().any(|op| !op.drained.is_empty() && matches!(op.phase, UpdatePhase::WaitingForSafePoint | UpdatePhase::Prepared | UpdatePhase::RecoveryRequired) && state.environment.scopes.get(&op.scope_id).is_none_or(|g| g.generation_id != op.generation.generation_id)) { return Err(UpdateError::Busy("native owner transition has not been published".into())); }
        Ok(UiCallAdmission { environment: state.environment.clone(), scope_id: scope.clone(), scope_generation_id: generation.generation_id.clone(), package: record.clone(), owner: self.inner.clone(), _fence: fence })
    }
    pub(crate) fn publication_guard(&self) -> Result<OwnedRwLockWriteGuard<()>> {
        self.inner.ui_fence.clone().try_write_owned().map_err(|_| UpdateError::Busy("UI dispatch acceptance still in progress".into()))
    }
    fn require_ui_writer(&self, admission: &UiCallAdmission) -> Result<()> {
        require(Arc::ptr_eq(&admission.owner, &self.inner) && matches!(&admission._fence, UiFence::Write { .. }), "environment change requires this coordinator's UI writer admission")
    }
    pub fn activate_with_ui_admission(&self, admission: &UiCallAdmission, operation_id: &str, now: u64) -> Result<UpdateOperation> {
        self.require_ui_writer(admission)?; self.activate_inner(operation_id, now)
    }
    pub fn set_ui_enabled_with_ui_admission(&self, admission: &UiCallAdmission, module_id: &str, enabled: bool, expected_generation_id: &str) -> Result<ModuleInventory> {
        self.require_ui_writer(admission)?; self.set_ui_enabled_inner(module_id, enabled, expected_generation_id)
    }
    pub fn apply_revocations_with_ui_admission(&self, admission: &UiCallAdmission, envelope: &mintcat_runtime_release::Signed<RevocationMetadata>, now: u64) -> Result<()> {
        self.require_ui_writer(admission)?; self.apply_revocations_inner(envelope, now)
    }
    pub fn rescue_status(&self) -> Result<(Option<String>, BTreeMap<String, String>)> {
        let state = self.read_state()?; Ok((state.recovery_required, state.scope_blockers))
    }
}
