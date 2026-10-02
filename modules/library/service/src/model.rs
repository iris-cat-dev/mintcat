use serde::{Deserialize, Serialize};

pub const GLOBAL_SETTING_SCOPE: &str = "global";
pub const LIBRARY_BACKUP_FORMAT: &str = "mintcat.library/1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameDefinition {
    pub game_id: String,
    pub game_key: String,
    pub display_name: String,
    pub icon: String,
    pub active: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameInstallation {
    pub installation_id: String,
    pub game_id: String,
    pub root_path: Option<String>,
    pub state: String,
    pub incarnation_id: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileRecord {
    pub profile_id: String,
    pub game_id: String,
    pub owner_scope: String,
    pub name: String,
    pub display_name: String,
    pub active: bool,
    pub description: String,
    pub revision: u64,
    pub content_revision: u64,
    pub lease_epoch: u64,
    pub incarnation_id: String,
    pub last_used_at_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupRecord {
    pub group_id: String,
    pub profile_id: String,
    pub parent_group_id: Option<String>,
    pub name: String,
    pub group_type: String,
    pub sort_order: u64,
    pub expanded: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryMod {
    pub mod_id: String,
    pub game_id: String,
    pub source_id: String,
    pub remote_game_key: String,
    pub remote_mod_id: String,
    pub revision: u64,
    pub incarnation_id: String,
    pub name_id: String,
    pub display_name: String,
    pub original_name: String,
    pub url: String,
    pub tags: Vec<String>,
    pub approval_status: String,
    pub dependency_mod_id: Option<String>,
    pub last_update_at_ms: i64,
    pub online_update_at_ms: i64,
    pub online_available: bool,
    pub local_missing: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModContentVersion {
    pub mod_id: String,
    pub version_id: String,
    pub remote_file_id: Option<String>,
    pub artifact_digest: Option<String>,
    pub legacy_location: Option<String>,
    pub availability: String,
    pub current: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub source_url: Option<String>,
    pub file_size: u64,
    pub legacy_download_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileModRecord {
    pub profile_mod_id: String,
    pub profile_id: String,
    pub mod_id: String,
    pub group_id: Option<String>,
    pub incarnation_id: String,
    pub sort_order: u64,
    pub enabled: bool,
    pub used_version: Option<String>,
    pub version_selection_kind: String,
    pub selected_version_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingRecord {
    pub scope_id: String,
    pub name: String,
    pub value: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialRecord {
    pub credential_id: String,
    pub owner_scope: String,
    pub provider: String,
    pub credential_ref: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyInstallHint {
    pub game_id: String,
    pub installed_hash: String,
    pub claim_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyIdMapping {
    pub source_digest: String,
    pub entity_kind: String,
    pub legacy_id: String,
    pub new_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryCounts {
    pub games: u64,
    pub installations: u64,
    pub profiles: u64,
    pub groups: u64,
    pub mods: u64,
    pub versions: u64,
    pub profile_mods: u64,
    pub settings: u64,
    pub credentials: u64,
    pub legacy_install_hints: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportRunRecord {
    pub source_digest: String,
    pub source_label: String,
    pub imported_at_ms: i64,
    pub counts: LibraryCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadOperationRecord {
    pub operation_id: String,
    pub owner_scope: String,
    pub idempotency_scope: String,
    pub idempotency_key: String,
    pub inputs_hash: String,
    pub profile_id: String,
    pub mode: String,
    pub expected_content_revision: u64,
    pub target_incarnation_id: String,
    pub owner_generation: String,
    pub fence_epoch: u64,
    pub state: String,
    pub result_json: Option<String>,
    pub error_code: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationLeaseRecord {
    pub resource_key: String,
    pub operation_id: String,
    pub fence_epoch: u64,
    pub owner_generation: String,
    pub state: String,
    pub acquired_at_ms: i64,
    pub heartbeat_at_ms: i64,
    pub released_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadOperationItemRecord {
    pub operation_id: String,
    pub item_id: String,
    pub profile_mod_id: String,
    pub profile_mod_incarnation_id: String,
    pub mod_id: String,
    pub source_id: String,
    pub source_mod_id: String,
    pub remote_game_key: String,
    pub source_file_id: Option<String>,
    pub resolved_version: String,
    pub expected_digest: Option<String>,
    pub validator: Option<String>,
    pub artifact_digest: Option<String>,
    #[serde(default)]
    pub apply_selection_kind: Option<String>,
    #[serde(default)]
    pub apply_selected_version_id: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContentRefRecord {
    pub digest: String,
    pub size: u64,
    pub verified: bool,
    pub grant_id: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileContentBindingRecord {
    pub profile_mod_id: String,
    pub version_id: String,
    pub artifact_digest: String,
    pub operation_id: String,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MutationResultRecord {
    pub owner_scope: String,
    pub idempotency_scope: String,
    pub idempotency_key: String,
    pub inputs_hash: String,
    pub result_json: String,
    pub operation_id: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboxRecord {
    pub event_seq: u64,
    pub event_id: String,
    pub operation_id: Option<String>,
    pub event_type: String,
    pub destination: String,
    pub payload_json: String,
    pub delivery_state: String,
    pub delivery_ref: Option<String>,
    pub created_at_ms: i64,
    pub delivered_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibrarySnapshot {
    pub games: Vec<GameDefinition>,
    pub installations: Vec<GameInstallation>,
    pub profiles: Vec<ProfileRecord>,
    pub groups: Vec<GroupRecord>,
    pub mods: Vec<LibraryMod>,
    pub versions: Vec<ModContentVersion>,
    pub profile_mods: Vec<ProfileModRecord>,
    pub settings: Vec<SettingRecord>,
    pub credentials: Vec<CredentialRecord>,
    pub legacy_install_hints: Vec<LegacyInstallHint>,
    pub import_runs: Vec<ImportRunRecord>,
    pub legacy_mappings: Vec<LegacyIdMapping>,
    #[serde(default)]
    pub download_operations: Vec<DownloadOperationRecord>,
    #[serde(default)]
    pub operation_leases: Vec<OperationLeaseRecord>,
    #[serde(default)]
    pub download_operation_items: Vec<DownloadOperationItemRecord>,
    #[serde(default)]
    pub content_refs: Vec<ContentRefRecord>,
    #[serde(default)]
    pub profile_content_bindings: Vec<ProfileContentBindingRecord>,
    #[serde(default)]
    pub mutation_results: Vec<MutationResultRecord>,
    #[serde(default)]
    pub outbox: Vec<OutboxRecord>,
}

impl LibrarySnapshot {
    pub fn counts(&self) -> LibraryCounts {
        LibraryCounts {
            games: self.games.len() as u64,
            installations: self.installations.len() as u64,
            profiles: self.profiles.len() as u64,
            groups: self.groups.len() as u64,
            mods: self.mods.len() as u64,
            versions: self.versions.len() as u64,
            profile_mods: self.profile_mods.len() as u64,
            settings: self.settings.len() as u64,
            credentials: self.credentials.len() as u64,
            legacy_install_hints: self.legacy_install_hints.len() as u64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryBackup {
    pub format: String,
    pub exported_at_ms: i64,
    pub snapshot: LibrarySnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyImportReport {
    pub source_digest: String,
    pub already_imported: bool,
    pub counts: LibraryCounts,
    pub imported_credentials: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct LegacyCredential<'a> {
    pub owner_scope: &'a str,
    pub provider: &'a str,
    pub secret: &'a str,
}

pub trait CredentialVault {
    fn store(&self, credential: LegacyCredential<'_>) -> Result<String, String>;
    fn delete(&self, credential_ref: &str) -> Result<(), String>;
}
