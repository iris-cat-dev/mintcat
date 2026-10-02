mod guard_recovery;
mod backup_boundary;
mod legacy;
mod library_api;
mod model;
mod operation_gate;
mod schema;
mod store;
mod client;
mod native;
mod manifest;
mod source_add;
mod import_outcome;
mod digest;
mod settings;
pub use import_outcome::LegacyImportOutcome;
pub use source_add::{SourceAddRequest, SourceAddTarget, SourceAddMetadata, SourceAddVersion, SourceAddResult};
pub use client::{LibraryStore, NativeOwnerIdentity};
pub use manifest::module_manifest;
pub use native::{run_native_owner, LIBRARY_CONTRACT_ID, LIBRARY_CONTRACT_VERSION};
pub use library_api::{GameEnrollmentRequest, GameEnrollmentResult};
pub(crate) use store::OwnerStore;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

use thiserror::Error;

pub use model::{
    ContentRefRecord, CredentialRecord, CredentialVault, DownloadOperationItemRecord,
    DownloadOperationRecord, GameDefinition, GameInstallation, GroupRecord, ImportRunRecord,
    LegacyCredential, LegacyIdMapping, LegacyImportReport, LegacyInstallHint, LibraryBackup,
    LibraryCounts, LibraryMod, LibrarySnapshot, ModContentVersion, MutationResultRecord,
    OperationLeaseRecord, OutboxRecord, ProfileContentBindingRecord, ProfileModRecord,
    ProfileRecord, SettingRecord, GLOBAL_SETTING_SCOPE, LIBRARY_BACKUP_FORMAT,
};
pub use operation_gate::{
    AppliedArtifact, ApplyOperationResult, CancelOperationResult, DeleteProfileMod,
    DownloadExecutionItem, DownloadExecutionPlan, DownloadOperationSnapshot,
    DownloadOperationState, FailedDownloadOperation, LibraryAuthorization, MutationResult,
    OutboxEvent, ProfileContentCapability, ProfileContentGuard, ProfileDownloadState,
    RecordedArtifact, SetProfileModEnabled, StartDownloadOperation, StartOperationItem,
    StartedDownloadOperation, CONTENT_WRITE_PERMISSION, METADATA_WRITE_PERMISSION,
    PROFILE_WRITE_PERMISSION, READ_PERMISSION,
};

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("runtime layout is not initialized")]
    UninitializedLayout,
    #[error("MODULE_UNAVAILABLE: Library signed owner unavailable: {0}")]
    OwnerUnavailable(String),
    #[error("Library database failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Library database mutex is poisoned")]
    Poisoned,
    #[error("Library data schema version {0} is unsupported")]
    UnsupportedSchema(u32),
    #[error("Library JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Library file operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("legacy database is missing required table {0}")]
    MissingLegacyTable(String),
    #[error("legacy database integrity check failed: {0}")]
    LegacyIntegrity(String),
    #[error("legacy row references missing {entity} id {legacy_id}")]
    MissingLegacyReference {
        entity: &'static str,
        legacy_id: i64,
    },
    #[error("Library resource was not found: {0}")]
    NotFound(String),
    #[error("Library permission is denied: {0}")]
    PermissionDenied(String),
    #[error("Library revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("Library resource is busy under operation {owner_operation_id}")]
    ResourceBusy { owner_operation_id: String },
    #[error("Library idempotency key was reused with different input")]
    IdempotencyConflict,
    #[error("Library operation is stale")]
    StaleOperation,
    #[error("Library fence is stale: expected {expected}, actual {actual}")]
    StaleFence { expected: u64, actual: u64 },
    #[error("Library target is stale: {0}")]
    StaleTarget(String),
    #[error("Library operation cannot proceed from state {0}")]
    OperationState(String),
    #[error("artifact is not verified and leased: {0}")]
    ArtifactNotVerified(String),
    #[error("Library already contains data from a different source")]
    LibraryNotEmpty,
    #[error("credential vault rejected legacy provider {provider}: {message}")]
    CredentialVault { provider: String, message: String },
    #[error("credential cleanup failed for {credential_ref}: {message}")]
    CredentialCleanup {
        credential_ref: String,
        message: String,
    },
    #[error("backup target already exists: {0}")]
    BackupExists(PathBuf),
    #[error("unsupported Library backup format {0}")]
    UnsupportedBackup(String),
    #[error("invalid Library record: {0}")]
    InvalidRecord(String),
}
