use std::collections::{BTreeMap, BTreeSet};
use mintcat_runtime_release::{RepositoryBundle, Signed, TargetsMetadata};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UpdatePhase { MetadataVerified, Resolved, Downloading, Staged, WaitingForSafePoint, Prepared, Probation, Confirmed, Rejected, RolledBack, RecoveryRequired }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeGeneration {
    pub generation_id: String,
    pub revision: u64,
    pub targets: Signed<TargetsMetadata>,
    pub verified_root: Signed<mintcat_runtime_release::RootMetadata>,
    pub confirmed: bool,
    pub new_writes: bool,
}
impl ScopeGeneration {
    pub fn digests(&self) -> BTreeSet<String> { self.targets.signed.packages.keys().cloned().collect() }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOperation {
    pub operation_id: String,
    pub scope_id: String,
    pub expected_scope_revision: u64,
    pub generation: ScopeGeneration,
    pub previous: Option<ScopeGeneration>,
    pub phase: UpdatePhase,
    pub failure: Option<String>,
    pub verified_at: u64,
    pub metadata_expires_at: u64,
    #[serde(default)] pub quarantined: bool,
    pub bundle: RepositoryBundle,
    /// Historical owner snapshots are retained only as recovery evidence.
    #[serde(default)] pub backups: BTreeMap<String, Value>,
    #[serde(default)] pub drained: BTreeSet<String>,
    #[serde(default)] pub migrated: BTreeSet<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSnapshot {
    pub generation_id: String,
    pub scopes: BTreeMap<String, ScopeGeneration>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LeaseKind { Task, Service, Rollback }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableLease {
    pub lease_id: String,
    pub owner_id: String,
    pub kind: LeaseKind,
    pub environment: EnvironmentSnapshot,
    pub digests: BTreeSet<String>,
    pub expires_at: Option<u64>,
    /// Expiry is not proof that a process/job stopped. Running leases require explicit release.
    pub running: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameInventory { pub game_id: String, pub display_name: String, pub descriptor: Value }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleInventoryEntry {
    pub module_id: String,
    pub digest: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")] pub game: Option<GameInventory>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleInventory { pub generation_id: String, pub modules: Vec<ModuleInventoryEntry> }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevocationMetadata {
    pub header: mintcat_runtime_release::MetadataHeader,
    pub digests: BTreeSet<String>,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UpdateState {
    #[serde(default)] pub schema_version: u64,
    /// Exact historical platform/source/UI proofs; never activated by the new runtime.
    #[serde(default)] pub archived_scopes: BTreeMap<String, ScopeGeneration>,
    pub environment: EnvironmentSnapshot,
    pub operations: BTreeMap<String, UpdateOperation>,
    pub leases: BTreeMap<String, DurableLease>,
    pub revoked: BTreeSet<String>,
    pub high_water: BTreeMap<String, (u64, String)>,
    #[serde(default)] pub revisions: BTreeMap<String, u64>,
    #[serde(default)] pub authorized_channels: BTreeMap<String, String>,
    #[serde(default)] pub scope_blockers: BTreeMap<String, String>,
    #[serde(default)] pub recovery_operation: Option<String>,
    #[serde(default)] pub revocation_high_water: Option<(u64, String, u64)>,
    pub recovery_required: Option<String>,
}
impl Default for UpdateState {
    fn default() -> Self { Self { schema_version: 2, archived_scopes: BTreeMap::new(), environment: EnvironmentSnapshot { generation_id: "empty".into(), scopes: BTreeMap::new() }, operations: BTreeMap::new(), leases: BTreeMap::new(), revoked: BTreeSet::new(), high_water: BTreeMap::new(), revisions: BTreeMap::new(), authorized_channels: BTreeMap::new(), scope_blockers: BTreeMap::new(), recovery_operation: None, revocation_high_water: None, recovery_required: None } }
}
