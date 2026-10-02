use std::fs::File;
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use mintcat_runtime_kernel::RuntimeLayout;
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};

use crate::{
    model::{
        ContentRefRecord, CredentialRecord, DownloadOperationItemRecord, DownloadOperationRecord,
        GameDefinition, GameInstallation, GroupRecord, ImportRunRecord, LegacyIdMapping,
        LegacyInstallHint, LibraryMod, LibrarySnapshot, ModContentVersion,
        MutationResultRecord, OperationLeaseRecord, OutboxRecord, ProfileContentBindingRecord,
        ProfileModRecord, ProfileRecord, SettingRecord, GLOBAL_SETTING_SCOPE,
    },
    schema::{MIGRATION_1, MIGRATION_2, MIGRATION_3, MIGRATION_4, MIGRATION_5},
    LibraryError,
};

#[derive(Clone)]
pub struct OwnerStore {
    path: PathBuf,
    connection: Arc<Mutex<Connection>>,
    _writer_lock: Option<Arc<File>>,
}

impl std::fmt::Debug for OwnerStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LibraryStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl OwnerStore {
    pub fn open(layout: &RuntimeLayout) -> Result<Self, LibraryError> {
        if !layout.initialized {
            return Err(LibraryError::UninitializedLayout);
        }
        let module_state = layout.state_dir.join("mintcat.library");
        fs::create_dir_all(&module_state)?;
        secure_state_directory(&module_state)?;
        let path = module_state.join("library.sqlite");
        let lock_path = module_state.join("writer.lock");
        let writer_lock = OpenOptions::new().read(true).write(true).create(true).open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&writer_lock)
            .map_err(|_| LibraryError::ResourceBusy { owner_operation_id: "library-writer".into() })?;
        if path.exists() {
            Self::read_only_health(&path, false)?;
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut connection = Connection::open_with_flags(&path, flags)?;
        secure_database_file(&path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS library_schema (
               version INTEGER PRIMARY KEY,
               applied_at_ms INTEGER NOT NULL
             );",
        )?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut version: u32 = transaction.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM library_schema",
            [],
            |row| row.get(0),
        )?;
        if version > 5 {
            return Err(LibraryError::UnsupportedSchema(version));
        }
        if version == 0 {
            transaction.execute_batch(MIGRATION_1)?;
            transaction.execute(
                "INSERT INTO library_schema(version, applied_at_ms)
                 VALUES (1, CAST(strftime('%s', 'now') AS INTEGER) * 1000)",
                [],
            )?;
            version = 1;
        }
        if version == 1 {
            transaction.execute_batch(MIGRATION_2)?;
            transaction.execute(
                "INSERT INTO library_schema(version, applied_at_ms)
                 VALUES (2, CAST(strftime('%s', 'now') AS INTEGER) * 1000)",
                [],
            )?;
            version = 2;
        }
        if version == 2 {
            transaction.execute_batch(MIGRATION_3)?;
            transaction.execute(
                "INSERT INTO library_schema(version, applied_at_ms)
                 VALUES (3, CAST(strftime('%s', 'now') AS INTEGER) * 1000)",
                [],
            )?;
            version = 3;
        }
        if version == 3 {
            transaction.execute_batch(MIGRATION_4)?;
            transaction.execute(
                "INSERT INTO library_schema(version, applied_at_ms)
                 VALUES (4, CAST(strftime('%s', 'now') AS INTEGER) * 1000)",
                [],
            )?;
            version = 4;
        }
        if version == 4 {
            transaction.execute_batch(MIGRATION_5)?;
            transaction.execute(
                "INSERT INTO library_schema(version, applied_at_ms)
                 VALUES (5, CAST(strftime('%s', 'now') AS INTEGER) * 1000)",
                [],
            )?;
        }
        transaction.commit()?;
        Ok(Self {
            path,
            connection: Arc::new(Mutex::new(connection)),
            _writer_lock: Some(Arc::new(writer_lock)),
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.path
    }
    /// This never initializes, migrates, recovers, or changes SQLite pragmas.
    pub(crate) fn read_only_health(path: &Path, require_current: bool) -> Result<(), LibraryError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        Self::check_read_only_health(&connection, require_current)
    }

    /// Retain the validated current-schema connection without acquiring writer ownership.
    pub(crate) fn open_read_only(path: &Path) -> Result<Self, LibraryError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        Self::check_read_only_health(&connection, true)?;
        Ok(Self {
            path: path.to_owned(),
            connection: Arc::new(Mutex::new(connection)),
            _writer_lock: None,
        })
    }

    fn check_read_only_health(connection: &Connection, require_current: bool) -> Result<(), LibraryError> {
        let version: u32 = connection.query_row("SELECT COALESCE(MAX(version), 0) FROM library_schema", [], |r| r.get(0))?;
        if version > 5 || version == 0 || (require_current && version != 5) {
            return Err(LibraryError::UnsupportedSchema(version));
        }
        let check: String = connection.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if check != "ok" { return Err(LibraryError::InvalidRecord("Library integrity check failed".into())); }
        Ok(())
    }
    pub(crate) fn paused_snapshot(path: &Path) -> Result<LibrarySnapshot, LibraryError> {
        Self::read_only_health(path, true)?;
        let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        crate::backup_boundary::ensure_quiescent(&transaction)?;
        let snapshot = read_snapshot(&transaction)?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>, LibraryError> {
        self.connection.lock().map_err(|_| LibraryError::Poisoned)
    }

    pub fn snapshot(&self) -> Result<LibrarySnapshot, LibraryError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let snapshot = read_snapshot(&transaction)?;
        transaction.commit()?;
        Ok(snapshot)
    }

    pub fn set_global_setting(&self, name: &str, value: &str) -> Result<(), LibraryError> {
        self.set_setting(GLOBAL_SETTING_SCOPE, name, value)
    }

    pub fn set_game_setting(
        &self,
        game_id: &str,
        name: &str,
        value: &str,
    ) -> Result<(), LibraryError> {
        if name.is_empty() {
            return Err(LibraryError::InvalidRecord(
                "setting name must be non-empty".to_owned(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM library_games WHERE game_id = ?1)",
            [game_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(LibraryError::InvalidRecord(format!(
                "unknown game setting scope {game_id}"
            )));
        }
        let now = now_ms();
        transaction.execute(
            "INSERT INTO library_settings(scope_id, name, value, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(scope_id, name) DO UPDATE SET value = excluded.value,
               updated_at_ms = excluded.updated_at_ms",
            params![game_id, name, value, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn get_global_setting(&self, name: &str) -> Result<Option<String>, LibraryError> {
        self.get_setting(GLOBAL_SETTING_SCOPE, name)
    }

    pub fn get_game_setting(
        &self,
        game_id: &str,
        name: &str,
    ) -> Result<Option<String>, LibraryError> {
        self.get_setting(game_id, name)
    }

    pub(crate) fn is_empty_transaction(
        transaction: &Transaction<'_>,
    ) -> Result<bool, rusqlite::Error> {
        let count: u64 = transaction.query_row(
            "SELECT
              (SELECT COUNT(*) FROM library_games) +
              (SELECT COUNT(*) FROM library_profiles) +
              (SELECT COUNT(*) FROM library_mods) +
              (SELECT COUNT(*) FROM library_import_runs)",
            [],
            |row| row.get(0),
        )?;
        Ok(count == 0)
    }

    fn set_setting(&self, scope_id: &str, name: &str, value: &str) -> Result<(), LibraryError> {
        if scope_id.is_empty() || name.is_empty() {
            return Err(LibraryError::InvalidRecord(
                "setting scope and name must be non-empty".to_owned(),
            ));
        }
        let now = now_ms();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO library_settings(scope_id, name, value, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(scope_id, name) DO UPDATE SET value = excluded.value,
               updated_at_ms = excluded.updated_at_ms",
            params![scope_id, name, value, now],
        )?;
        Ok(())
    }

    fn get_setting(&self, scope_id: &str, name: &str) -> Result<Option<String>, LibraryError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT value FROM library_settings WHERE scope_id = ?1 AND name = ?2",
                params![scope_id, name],
                |row| row.get(0),
            )
            .optional()?)
    }
}


pub(crate) fn validate_snapshot(snapshot: &LibrarySnapshot) -> Result<(), LibraryError> {
    for value in snapshot
        .games
        .iter()
        .map(|record| (&record.game_id, "game id"))
        .chain(
            snapshot
                .profiles
                .iter()
                .map(|record| (&record.profile_id, "profile id")),
        )
        .chain(
            snapshot
                .mods
                .iter()
                .map(|record| (&record.mod_id, "mod id")),
        )
    {
        if value.0.is_empty() {
            return Err(LibraryError::InvalidRecord(format!(
                "{} must be non-empty",
                value.1
            )));
        }
    }
    for version in &snapshot.versions {
        if let Some(digest) = &version.artifact_digest {
            let valid = digest.len() == 71
                && digest.starts_with("sha256:")
                && digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit());
            if !valid {
                return Err(LibraryError::InvalidRecord(format!(
                    "invalid artifact digest {digest}"
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn read_snapshot(transaction: &Transaction<'_>) -> Result<LibrarySnapshot, LibraryError> {
    Ok(LibrarySnapshot {
        games: query_games(transaction)?,
        installations: query_installations(transaction)?,
        profiles: query_profiles(transaction)?,
        groups: query_groups(transaction)?,
        mods: query_mods(transaction)?,
        versions: query_versions(transaction)?,
        profile_mods: query_profile_mods(transaction)?,
        settings: query_settings(transaction)?,
        credentials: query_credentials(transaction)?,
        legacy_install_hints: query_install_hints(transaction)?,
        import_runs: query_import_runs(transaction)?,
        legacy_mappings: query_mappings(transaction)?,
        download_operations: query_download_operations(transaction)?,
        operation_leases: query_operation_leases(transaction)?,
        download_operation_items: query_download_operation_items(transaction)?,
        content_refs: query_content_refs(transaction)?,
        profile_content_bindings: query_profile_content_bindings(transaction)?,
        mutation_results: query_mutation_results(transaction)?,
        outbox: query_outbox(transaction)?,
    })
}

fn query_games(transaction: &Transaction<'_>) -> Result<Vec<GameDefinition>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT game_id, game_key, display_name, icon, active, created_at_ms, updated_at_ms
         FROM library_games ORDER BY game_id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(GameDefinition {
                game_id: row.get(0)?,
                game_key: row.get(1)?,
                display_name: row.get(2)?,
                icon: row.get(3)?,
                active: row.get(4)?,
                created_at_ms: row.get(5)?,
                updated_at_ms: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn query_installations(
    transaction: &Transaction<'_>,
) -> Result<Vec<GameInstallation>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT installation_id, game_id, root_path, state, incarnation_id, created_at_ms,
                updated_at_ms FROM library_installations ORDER BY installation_id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(GameInstallation {
                installation_id: row.get(0)?,
                game_id: row.get(1)?,
                root_path: row.get(2)?,
                state: row.get(3)?,
                incarnation_id: row.get(4)?,
                created_at_ms: row.get(5)?,
                updated_at_ms: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn query_profiles(transaction: &Transaction<'_>) -> Result<Vec<ProfileRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT profile_id, game_id, owner_scope, name, display_name, active, description,
                revision, content_revision, lease_epoch, incarnation_id, last_used_at_ms,
                created_at_ms, updated_at_ms, deleted_at_ms
         FROM library_profiles ORDER BY profile_id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(ProfileRecord {
                profile_id: row.get(0)?,
                game_id: row.get(1)?,
                owner_scope: row.get(2)?,
                name: row.get(3)?,
                display_name: row.get(4)?,
                active: row.get(5)?,
                description: row.get(6)?,
                revision: row.get(7)?,
                content_revision: row.get(8)?,
                lease_epoch: row.get(9)?,
                incarnation_id: row.get(10)?,
                last_used_at_ms: row.get(11)?,
                created_at_ms: row.get(12)?,
                updated_at_ms: row.get(13)?,
                deleted_at_ms: row.get(14)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn query_groups(transaction: &Transaction<'_>) -> Result<Vec<GroupRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT group_id, profile_id, parent_group_id, name, group_type, sort_order, expanded,
                created_at_ms, updated_at_ms FROM library_groups ORDER BY profile_id, sort_order, group_id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(GroupRecord {
                group_id: row.get(0)?,
                profile_id: row.get(1)?,
                parent_group_id: row.get(2)?,
                name: row.get(3)?,
                group_type: row.get(4)?,
                sort_order: row.get(5)?,
                expanded: row.get(6)?,
                created_at_ms: row.get(7)?,
                updated_at_ms: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn query_mods(transaction: &Transaction<'_>) -> Result<Vec<LibraryMod>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT mod_id, game_id, source_id, remote_game_key, remote_mod_id, revision,
                incarnation_id, name_id, display_name, original_name, url, tags_json,
                approval_status, last_update_at_ms, online_update_at_ms, online_available,
                local_missing, dependency_mod_id, created_at_ms, updated_at_ms
         FROM library_mods ORDER BY mod_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, u64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, String>(12)?,
            row.get::<_, i64>(13)?,
            row.get::<_, i64>(14)?,
            row.get::<_, bool>(15)?,
            row.get::<_, bool>(16)?,
            row.get::<_, Option<String>>(17)?,
            row.get::<_, i64>(18)?,
            row.get::<_, i64>(19)?,
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        let (
            mod_id,
            game_id,
            source_id,
            remote_game_key,
            remote_mod_id,
            revision,
            incarnation_id,
            name_id,
            display_name,
            original_name,
            url,
            tags_json,
            approval_status,
            last_update_at_ms,
            online_update_at_ms,
            online_available,
            local_missing,
            dependency_mod_id,
            created_at_ms,
            updated_at_ms,
        ) = row?;
        result.push(LibraryMod {
            mod_id,
            game_id,
            source_id,
            remote_game_key,
            remote_mod_id,
            revision,
            incarnation_id,
            name_id,
            display_name,
            original_name,
            url,
            tags: serde_json::from_str(&tags_json)?,
            approval_status,
            dependency_mod_id,
            last_update_at_ms,
            online_update_at_ms,
            online_available,
            local_missing,
            created_at_ms,
            updated_at_ms,
        });
    }
    Ok(result)
}

fn query_versions(transaction: &Transaction<'_>) -> Result<Vec<ModContentVersion>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT mod_id, version_id, remote_file_id, artifact_digest, legacy_location,
                source_url, file_size, legacy_download_status, availability, current,
                created_at_ms, updated_at_ms
         FROM library_mod_versions ORDER BY mod_id, version_id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(ModContentVersion {
                mod_id: row.get(0)?,
                version_id: row.get(1)?,
                remote_file_id: row.get(2)?,
                artifact_digest: row.get(3)?,
                legacy_location: row.get(4)?,
                source_url: row.get(5)?,
                file_size: row.get(6)?,
                legacy_download_status: row.get(7)?,
                availability: row.get(8)?,
                current: row.get(9)?,
                created_at_ms: row.get(10)?,
                updated_at_ms: row.get(11)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn query_profile_mods(
    transaction: &Transaction<'_>,
) -> Result<Vec<ProfileModRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT profile_mod_id, profile_id, mod_id, group_id, incarnation_id, sort_order,
                enabled, used_version, version_selection_kind, selected_version_id,
                created_at_ms, updated_at_ms
         FROM library_profile_mods ORDER BY profile_id, sort_order, profile_mod_id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(ProfileModRecord {
                profile_mod_id: row.get(0)?,
                profile_id: row.get(1)?,
                mod_id: row.get(2)?,
                group_id: row.get(3)?,
                incarnation_id: row.get(4)?,
                sort_order: row.get(5)?,
                enabled: row.get(6)?,
                used_version: row.get(7)?,
                version_selection_kind: row.get(8)?,
                selected_version_id: row.get(9)?,
                created_at_ms: row.get(10)?,
                updated_at_ms: row.get(11)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn query_settings(transaction: &Transaction<'_>) -> Result<Vec<SettingRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT scope_id, name, value, created_at_ms, updated_at_ms
         FROM library_settings ORDER BY scope_id, name",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(SettingRecord {
                scope_id: row.get(0)?,
                name: row.get(1)?,
                value: row.get(2)?,
                created_at_ms: row.get(3)?,
                updated_at_ms: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn query_credentials(transaction: &Transaction<'_>) -> Result<Vec<CredentialRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT credential_id, owner_scope, provider, credential_ref, created_at_ms, updated_at_ms
         FROM library_credentials ORDER BY credential_id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(CredentialRecord {
                credential_id: row.get(0)?,
                owner_scope: row.get(1)?,
                provider: row.get(2)?,
                credential_ref: row.get(3)?,
                created_at_ms: row.get(4)?,
                updated_at_ms: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn query_install_hints(
    transaction: &Transaction<'_>,
) -> Result<Vec<LegacyInstallHint>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT game_id, installed_hash, claim_status
         FROM library_legacy_install_hints ORDER BY game_id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(LegacyInstallHint {
                game_id: row.get(0)?,
                installed_hash: row.get(1)?,
                claim_status: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn query_import_runs(transaction: &Transaction<'_>) -> Result<Vec<ImportRunRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT source_digest, source_label, imported_at_ms, row_counts_json
         FROM library_import_runs ORDER BY source_digest",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        let (source_digest, source_label, imported_at_ms, counts_json) = row?;
        result.push(ImportRunRecord {
            source_digest,
            source_label,
            imported_at_ms,
            counts: serde_json::from_str(&counts_json)?,
        });
    }
    Ok(result)
}

fn query_mappings(transaction: &Transaction<'_>) -> Result<Vec<LegacyIdMapping>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT source_digest, entity_kind, legacy_id, new_id
         FROM library_legacy_mappings ORDER BY source_digest, entity_kind, legacy_id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(LegacyIdMapping {
                source_digest: row.get(0)?,
                entity_kind: row.get(1)?,
                legacy_id: row.get(2)?,
                new_id: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn query_download_operations(
    transaction: &Transaction<'_>,
) -> Result<Vec<DownloadOperationRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT operation_id, owner_scope, idempotency_scope, idempotency_key, inputs_hash,
                profile_id, mode, expected_content_revision, target_incarnation_id,
                owner_generation, fence_epoch, state, result_json, error_code,
                created_at_ms, updated_at_ms, completed_at_ms
         FROM library_download_operations ORDER BY operation_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(DownloadOperationRecord {
            operation_id: row.get(0)?,
            owner_scope: row.get(1)?,
            idempotency_scope: row.get(2)?,
            idempotency_key: row.get(3)?,
            inputs_hash: row.get(4)?,
            profile_id: row.get(5)?,
            mode: row.get(6)?,
            expected_content_revision: row.get(7)?,
            target_incarnation_id: row.get(8)?,
            owner_generation: row.get(9)?,
            fence_epoch: row.get(10)?,
            state: row.get(11)?,
            result_json: row.get(12)?,
            error_code: row.get(13)?,
            created_at_ms: row.get(14)?,
            updated_at_ms: row.get(15)?,
            completed_at_ms: row.get(16)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn query_operation_leases(
    transaction: &Transaction<'_>,
) -> Result<Vec<OperationLeaseRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT resource_key, operation_id, fence_epoch, owner_generation, state,
                acquired_at_ms, heartbeat_at_ms, released_at_ms
         FROM library_operation_leases ORDER BY resource_key",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(OperationLeaseRecord {
            resource_key: row.get(0)?,
            operation_id: row.get(1)?,
            fence_epoch: row.get(2)?,
            owner_generation: row.get(3)?,
            state: row.get(4)?,
            acquired_at_ms: row.get(5)?,
            heartbeat_at_ms: row.get(6)?,
            released_at_ms: row.get(7)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn query_download_operation_items(
    transaction: &Transaction<'_>,
) -> Result<Vec<DownloadOperationItemRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT operation_id, item_id, profile_mod_id, profile_mod_incarnation_id,
                mod_id, source_id, source_mod_id, remote_game_key, source_file_id,
                resolved_version, expected_digest, validator, artifact_digest,
                apply_selection_kind, apply_selected_version_id, state
         FROM library_download_operation_items ORDER BY operation_id, item_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(DownloadOperationItemRecord {
            operation_id: row.get(0)?,
            item_id: row.get(1)?,
            profile_mod_id: row.get(2)?,
            profile_mod_incarnation_id: row.get(3)?,
            mod_id: row.get(4)?,
            source_id: row.get(5)?,
            source_mod_id: row.get(6)?,
            remote_game_key: row.get(7)?,
            source_file_id: row.get(8)?,
            resolved_version: row.get(9)?,
            expected_digest: row.get(10)?,
            validator: row.get(11)?,
            artifact_digest: row.get(12)?,
            apply_selection_kind: row.get(13)?,
            apply_selected_version_id: row.get(14)?,
            state: row.get(15)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn query_content_refs(
    transaction: &Transaction<'_>,
) -> Result<Vec<ContentRefRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT digest, size, verified, grant_id, created_at_ms
         FROM library_content_refs ORDER BY digest",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(ContentRefRecord {
            digest: row.get(0)?,
            size: row.get(1)?,
            verified: row.get(2)?,
            grant_id: row.get(3)?,
            created_at_ms: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn query_profile_content_bindings(
    transaction: &Transaction<'_>,
) -> Result<Vec<ProfileContentBindingRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT profile_mod_id, version_id, artifact_digest, operation_id, updated_at_ms
         FROM library_profile_content_bindings ORDER BY profile_mod_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(ProfileContentBindingRecord {
            profile_mod_id: row.get(0)?,
            version_id: row.get(1)?,
            artifact_digest: row.get(2)?,
            operation_id: row.get(3)?,
            updated_at_ms: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn query_mutation_results(
    transaction: &Transaction<'_>,
) -> Result<Vec<MutationResultRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT owner_scope, idempotency_scope, idempotency_key, inputs_hash,
                result_json, operation_id, created_at_ms
         FROM library_mutation_results
         ORDER BY owner_scope, idempotency_scope, idempotency_key",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(MutationResultRecord {
            owner_scope: row.get(0)?,
            idempotency_scope: row.get(1)?,
            idempotency_key: row.get(2)?,
            inputs_hash: row.get(3)?,
            result_json: row.get(4)?,
            operation_id: row.get(5)?,
            created_at_ms: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn query_outbox(transaction: &Transaction<'_>) -> Result<Vec<OutboxRecord>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT event_seq, event_id, operation_id, event_type, destination, payload_json,
                delivery_state, delivery_ref, created_at_ms, delivered_at_ms
         FROM library_outbox ORDER BY event_seq",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(OutboxRecord {
            event_seq: row.get(0)?,
            event_id: row.get(1)?,
            operation_id: row.get(2)?,
            event_type: row.get(3)?,
            destination: row.get(4)?,
            payload_json: row.get(5)?,
            delivery_state: row.get(6)?,
            delivery_ref: row.get(7)?,
            created_at_ms: row.get(8)?,
            delivered_at_ms: row.get(9)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub(crate) fn insert_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &LibrarySnapshot,
) -> Result<(), LibraryError> {
    for record in &snapshot.games {
        transaction.execute(
            "INSERT INTO library_games VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.game_id,
                record.game_key,
                record.display_name,
                record.icon,
                record.active,
                record.created_at_ms,
                record.updated_at_ms
            ],
        )?;
    }
    for record in &snapshot.installations {
        transaction.execute(
            "INSERT INTO library_installations VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.installation_id,
                record.game_id,
                record.root_path,
                record.state,
                record.incarnation_id,
                record.created_at_ms,
                record.updated_at_ms
            ],
        )?;
    }
    for record in &snapshot.profiles {
        transaction.execute(
            "INSERT INTO library_profiles(
               profile_id, game_id, owner_scope, name, display_name, active, description,
               revision, content_revision, lease_epoch, incarnation_id, last_used_at_ms,
               created_at_ms, updated_at_ms, deleted_at_ms
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
             )",
            params![
                record.profile_id,
                record.game_id,
                record.owner_scope,
                record.name,
                record.display_name,
                record.active,
                record.description,
                record.revision,
                record.content_revision,
                record.lease_epoch,
                record.incarnation_id,
                record.last_used_at_ms,
                record.created_at_ms,
                record.updated_at_ms,
                record.deleted_at_ms
            ],
        )?;
    }
    for record in &snapshot.groups {
        transaction.execute(
            "INSERT INTO library_groups VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.group_id,
                record.profile_id,
                record.parent_group_id,
                record.name,
                record.group_type,
                record.sort_order,
                record.expanded,
                record.created_at_ms,
                record.updated_at_ms
            ],
        )?;
    }
    for record in &snapshot.mods {
        transaction.execute(
            "INSERT INTO library_mods(
               mod_id, game_id, source_id, remote_game_key, remote_mod_id, revision,
               incarnation_id, name_id, display_name, original_name, url, tags_json,
               approval_status, last_update_at_ms, online_update_at_ms, online_available,
               local_missing, dependency_mod_id, created_at_ms, updated_at_ms
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
               ?15, ?16, ?17, ?18, ?19, ?20
             )",
            params![
                record.mod_id,
                record.game_id,
                record.source_id,
                record.remote_game_key,
                record.remote_mod_id,
                record.revision,
                record.incarnation_id,
                record.name_id,
                record.display_name,
                record.original_name,
                record.url,
                serde_json::to_string(&record.tags)?,
                record.approval_status,
                record.last_update_at_ms,
                record.online_update_at_ms,
                record.online_available,
                record.local_missing,
                record.dependency_mod_id,
                record.created_at_ms,
                record.updated_at_ms
            ],
        )?;
    }
    for record in &snapshot.versions {
        transaction.execute(
            "INSERT INTO library_mod_versions VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.mod_id,
                record.version_id,
                record.remote_file_id,
                record.artifact_digest,
                record.legacy_location,
                record.source_url,
                record.file_size,
                record.legacy_download_status,
                record.availability,
                record.current,
                record.created_at_ms,
                record.updated_at_ms
            ],
        )?;
    }
    for record in &snapshot.profile_mods {
        transaction.execute(
            "INSERT INTO library_profile_mods(
               profile_mod_id, profile_id, mod_id, group_id, incarnation_id, sort_order,
               enabled, used_version, version_selection_kind, selected_version_id,
               created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                record.profile_mod_id,
                record.profile_id,
                record.mod_id,
                record.group_id,
                record.incarnation_id,
                record.sort_order,
                record.enabled,
                record.used_version,
                record.version_selection_kind,
                record.selected_version_id,
                record.created_at_ms,
                record.updated_at_ms
            ],
        )?;
    }
    for record in &snapshot.settings {
        transaction.execute(
            "INSERT INTO library_settings VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                record.scope_id,
                record.name,
                record.value,
                record.created_at_ms,
                record.updated_at_ms
            ],
        )?;
    }
    for record in &snapshot.credentials {
        transaction.execute(
            "INSERT INTO library_credentials VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.credential_id,
                record.owner_scope,
                record.provider,
                record.credential_ref,
                record.created_at_ms,
                record.updated_at_ms
            ],
        )?;
    }
    for record in &snapshot.legacy_install_hints {
        transaction.execute(
            "INSERT INTO library_legacy_install_hints VALUES (?1, ?2, ?3)",
            params![record.game_id, record.installed_hash, record.claim_status],
        )?;
    }
    for record in &snapshot.import_runs {
        transaction.execute(
            "INSERT INTO library_import_runs VALUES (?1, ?2, ?3, ?4)",
            params![
                record.source_digest,
                record.source_label,
                record.imported_at_ms,
                serde_json::to_string(&record.counts)?
            ],
        )?;
    }
    for record in &snapshot.legacy_mappings {
        transaction.execute(
            "INSERT INTO library_legacy_mappings VALUES (?1, ?2, ?3, ?4)",
            params![
                record.source_digest,
                record.entity_kind,
                record.legacy_id,
                record.new_id
            ],
        )?;
    }
    for record in &snapshot.download_operations {
        transaction.execute(
            "INSERT INTO library_download_operations(
               operation_id, owner_scope, idempotency_scope, idempotency_key, inputs_hash,
               profile_id, mode, expected_content_revision, target_incarnation_id,
               owner_generation, fence_epoch, state, result_json, error_code,
               created_at_ms, updated_at_ms, completed_at_ms
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
               ?15, ?16, ?17
             )",
            params![
                record.operation_id,
                record.owner_scope,
                record.idempotency_scope,
                record.idempotency_key,
                record.inputs_hash,
                record.profile_id,
                record.mode,
                record.expected_content_revision,
                record.target_incarnation_id,
                record.owner_generation,
                record.fence_epoch,
                record.state,
                record.result_json,
                record.error_code,
                record.created_at_ms,
                record.updated_at_ms,
                record.completed_at_ms
            ],
        )?;
    }
    for record in &snapshot.operation_leases {
        transaction.execute(
            "INSERT INTO library_operation_leases(
               resource_key, operation_id, fence_epoch, owner_generation, state,
               acquired_at_ms, heartbeat_at_ms, released_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.resource_key,
                record.operation_id,
                record.fence_epoch,
                record.owner_generation,
                record.state,
                record.acquired_at_ms,
                record.heartbeat_at_ms,
                record.released_at_ms
            ],
        )?;
    }
    for record in &snapshot.download_operation_items {
        transaction.execute(
            "INSERT INTO library_download_operation_items(
               operation_id, item_id, profile_mod_id, profile_mod_incarnation_id,
               mod_id, source_id, source_mod_id, remote_game_key, source_file_id,
               resolved_version, expected_digest, validator, artifact_digest,
               apply_selection_kind, apply_selected_version_id, state
             ) VALUES (
               ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
             )",
            params![
                record.operation_id,
                record.item_id,
                record.profile_mod_id,
                record.profile_mod_incarnation_id,
                record.mod_id,
                record.source_id,
                record.source_mod_id,
                record.remote_game_key,
                record.source_file_id,
                record.resolved_version,
                record.expected_digest,
                record.validator,
                record.artifact_digest,
                record.apply_selection_kind,
                record.apply_selected_version_id,
                record.state
            ],
        )?;
    }
    for record in &snapshot.content_refs {
        transaction.execute(
            "INSERT INTO library_content_refs(
               digest, size, verified, grant_id, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                record.digest,
                record.size,
                record.verified,
                record.grant_id,
                record.created_at_ms
            ],
        )?;
    }
    for record in &snapshot.profile_content_bindings {
        transaction.execute(
            "INSERT INTO library_profile_content_bindings(
               profile_mod_id, version_id, artifact_digest, operation_id, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                record.profile_mod_id,
                record.version_id,
                record.artifact_digest,
                record.operation_id,
                record.updated_at_ms
            ],
        )?;
    }
    for record in &snapshot.mutation_results {
        transaction.execute(
            "INSERT INTO library_mutation_results(
               owner_scope, idempotency_scope, idempotency_key, inputs_hash,
               result_json, operation_id, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.owner_scope,
                record.idempotency_scope,
                record.idempotency_key,
                record.inputs_hash,
                record.result_json,
                record.operation_id,
                record.created_at_ms
            ],
        )?;
    }
    for record in &snapshot.outbox {
        transaction.execute(
            "INSERT INTO library_outbox(
               event_seq, event_id, operation_id, event_type, destination, payload_json,
               delivery_state, delivery_ref, created_at_ms, delivered_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.event_seq,
                record.event_id,
                record.operation_id,
                record.event_type,
                record.destination,
                record.payload_json,
                record.delivery_state,
                record.delivery_ref,
                record.created_at_ms,
                record.delivered_at_ms
            ],
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn secure_state_directory(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn secure_state_directory(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(unix)]
fn secure_database_file(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn secure_database_file(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

