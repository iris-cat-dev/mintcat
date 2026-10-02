use std::collections::BTreeMap;
use mintcat_library_service::LibrarySnapshot;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::BackupError;

/// Each owner produces a logical snapshot under the common maintenance admission.
/// This is not a cross-database transaction or a transplant of kernel authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerSnapshot {
    pub schema_version: u32,
    pub credential_policy: String,
    pub excluded_fields: Vec<String>,
    pub content: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableSnapshot {
    pub library: LibrarySnapshot,
    pub owners: BTreeMap<String, OwnerSnapshot>,
}
impl PortableSnapshot {
    pub fn counts(&self) -> mintcat_library_service::LibraryCounts { self.library.counts() }
    pub fn clear_credentials(&mut self) {
        for credential in &mut self.library.credentials { credential.credential_ref = None; }
    }
    /// Preserve completed content provenance, never portable execution/replay authority.
    pub fn exclude_execution_authority(&mut self) -> Result<(), BackupError> {
        if self.library.download_operations.iter().any(|operation| !["succeeded", "cancelled", "failed"].contains(&operation.state.as_str()))
            || self.library.operation_leases.iter().any(|lease| ["active", "suspect"].contains(&lease.state.as_str())) {
            return Err(BackupError::Busy("snapshot has unfinished downloads".into()));
        }
        self.library.outbox.clear();
        self.library.mutation_results.clear();
        self.library.operation_leases.clear();
        // Content bindings have an operation FK. Retain completed provenance in a
        // non-caller scope so an imported idempotency key cannot replay a live job.
        for operation in &mut self.library.download_operations {
            for scope in [&mut operation.owner_scope, &mut operation.idempotency_scope] {
                if scope.as_str() != "portable-history" { scope.clear(); scope.push_str("portable-history"); }
            }
        }
        Ok(())
    }
}
pub trait PortableOwners: Send + Sync {
    fn snapshot(&self) -> Result<BTreeMap<String, OwnerSnapshot>, BackupError>;
    fn ensure_empty(&self) -> Result<(), BackupError>;
    fn validate(&self, owners: &BTreeMap<String, OwnerSnapshot>) -> Result<(), BackupError>;
    fn write_epochs(&self) -> Result<BTreeMap<String, u64>, BackupError>;
    /// Imports only owners that are exactly empty, each in its owner's transaction.
    /// A partial commit stays behind the durable maintenance marker until recovery.
    fn import_empty(&self, owners: &BTreeMap<String, OwnerSnapshot>) -> Result<(), BackupError>;
}
