use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::{
    model::{
        CredentialRecord, CredentialVault, GameDefinition, GameInstallation, GroupRecord,
        ImportRunRecord, LegacyCredential, LegacyIdMapping, LegacyImportReport, LegacyInstallHint,
        LibraryMod, LibrarySnapshot, ModContentVersion, ProfileModRecord, ProfileRecord,
        SettingRecord, GLOBAL_SETTING_SCOPE,
    },
    store::{insert_snapshot, OwnerStore},
    LibraryError,
};

pub(crate) const OWNER_SCOPE: &str = "local-user";
const REQUIRED_TABLES: &[&str] = &[
    "games",
    "users",
    "oauths",
    "mods",
    "mod_versions",
    "mod_downloads",
    "mod_status",
    "profiles",
    "profile_folders",
    "profile_mods",
    "settings",
];

#[derive(Debug, Serialize)]
struct LegacyDump {
    games: Vec<LegacyGame>,
    users: Vec<LegacyUser>,
    oauths: Vec<LegacyOauth>,
    profiles: Vec<LegacyProfile>,
    folders: Vec<LegacyFolder>,
    mods: Vec<LegacyMod>,
    profile_mods: Vec<LegacyProfileMod>,
    settings: Vec<LegacySetting>,
}

#[derive(Debug, Serialize)]
struct LegacyGame {
    id: i64,
    name: String,
    display_name: String,
    icon: String,
    install_path: String,
    active: bool,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize)]
struct LegacyUser {
    id: i64,
    username: String,
}

#[derive(Debug, Serialize)]
struct LegacyOauth {
    id: i64,
    user_id: i64,
    token: String,
    platform: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize)]
struct LegacyProfile {
    id: i64,
    game_id: i64,
    user_id: i64,
    name: String,
    display_name: String,
    active: bool,
    description: String,
    last_used_at: i64,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize)]
struct LegacyFolder {
    id: i64,
    profile_id: i64,
    parent_id: Option<i64>,
    name: String,
    folder_type: String,
    sort_order: u64,
    expanded: bool,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize)]
struct LegacyMod {
    id: i64,
    platform_id: i64,
    game_id: i64,
    name_id: String,
    display_name: String,
    original_name: String,
    url: String,
    source_type: String,
    tags: Vec<String>,
    approval_status: String,
    dependency_id: i64,
    current_version: String,
    available_versions: Vec<String>,
    download_url: String,
    cache_path: String,
    file_size: u64,
    download_status: String,
    last_update_at: i64,
    online_update_at: i64,
    online_available: bool,
    local_missing: bool,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize)]
struct LegacyProfileMod {
    id: i64,
    profile_id: i64,
    mod_id: i64,
    folder_id: Option<i64>,
    sort_order: u64,
    enabled: bool,
    used_version: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize)]
struct LegacySetting {
    name: String,
    value: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug)]
struct StagedCredential {
    legacy_ids: Vec<i64>,
    provider: String,
    credential_ref: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl OwnerStore {
pub fn import_legacy(&self, source: &Path, vault: &dyn CredentialVault) -> Result<LegacyImportReport, LibraryError> {
    self.import_legacy_bound(source, None, vault)
}
pub fn import_legacy_checked(&self, source: &Path, expected_source_digest: &str, vault: &dyn CredentialVault) -> Result<LegacyImportReport, LibraryError> {
    self.import_legacy_bound(source, Some(expected_source_digest), vault)
}
fn import_legacy_bound(&self, source: &Path, expected_source_digest: Option<&str>, vault: &dyn CredentialVault) -> Result<LegacyImportReport, LibraryError> {
    let dump = read_legacy_dump(source)?;
    let legacy_users: HashSet<i64> = dump.users.iter().map(|user| user.id).collect();
    for oauth in &dump.oauths {
        if !legacy_users.contains(&oauth.user_id) {
            return Err(missing("user", oauth.user_id));
        }
    }
    let encoded = serde_json::to_vec(&dump)?;
    let source_digest = format!("sha256:{:x}", Sha256::digest(encoded));
    if expected_source_digest.is_some_and(|expected| expected != source_digest) {
        return Err(LibraryError::StaleOperation);
    }

    let existing: Option<String> = {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT row_counts_json FROM library_import_runs WHERE source_digest = ?1",
                [&source_digest],
                |row| row.get(0),
            )
            .optional()?
    };
    if existing.is_some() {
        let mut outcome = self.legacy_import_outcome(&source_digest)?.ok_or(LibraryError::StaleOperation)?;
        outcome.result.already_imported = true;
        return Ok(outcome.result);
    }

    let staged_credentials = stage_credentials(&dump.oauths, vault)?;
    let imported_credentials = staged_credentials
        .iter()
        .filter(|credential| credential.credential_ref.is_some())
        .count() as u64;
    let stored_refs: Vec<String> = staged_credentials
        .iter()
        .filter_map(|credential| credential.credential_ref.clone())
        .collect();
    let (mut snapshot, mut warnings) =
        match build_snapshot(&dump, &source_digest, staged_credentials) {
            Ok(result) => result,
            Err(error) => {
                cleanup_credentials(vault, &stored_refs)?;
                return Err(error);
            }
        };
    let counts = snapshot.counts();
    snapshot.import_runs.push(ImportRunRecord {
        source_digest: source_digest.clone(),
        source_label: source.display().to_string(),
        imported_at_ms: current_time_ms(),
        counts: counts.clone(),
    });
    if imported_credentials == 0 {
        warnings.push("legacy OAuth rows contained no credential secret".to_owned());
    }
    let report = LegacyImportReport { source_digest, already_imported: false, counts, imported_credentials, warnings };
    let result = (|| -> Result<(), LibraryError> {
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !OwnerStore::is_empty_transaction(&transaction)? {
            return Err(LibraryError::LibraryNotEmpty);
        }
        crate::import_outcome::advance(self.database_path())?;
        insert_snapshot(&transaction, &snapshot)?;
        crate::import_outcome::record(&transaction, self.database_path(), &report)?;
        transaction.commit()?;
        Ok(())
    })();
    if let Err(error) = result {
        cleanup_credentials(vault, &stored_refs)?;
        return Err(error);
    }

    Ok(report)
} }

pub(crate) fn legacy_source_digest(source: &Path) -> Result<String, LibraryError> {
    let dump = read_legacy_dump(source)?;
    Ok(format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&dump)?)))
}

fn read_legacy_dump(path: &Path) -> Result<LegacyDump, LibraryError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut connection = Connection::open_with_flags(path, flags)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch("PRAGMA query_only = ON; PRAGMA foreign_keys = ON;")?;
    for table in REQUIRED_TABLES {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(LibraryError::MissingLegacyTable((*table).to_owned()));
        }
    }
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(LibraryError::LegacyIntegrity(integrity));
    }

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let dump = LegacyDump {
        games: read_games(&transaction)?,
        users: read_users(&transaction)?,
        oauths: read_oauths(&transaction)?,
        profiles: read_profiles(&transaction)?,
        folders: read_folders(&transaction)?,
        mods: read_mods(&transaction)?,
        profile_mods: read_profile_mods(&transaction)?,
        settings: read_settings(&transaction)?,
    };
    transaction.commit()?;
    Ok(dump)
}

fn read_games(connection: &Connection) -> Result<Vec<LegacyGame>, LibraryError> {
    let mut statement = connection.prepare(
        "SELECT id, name, display_name, icon, install_path, is_active, created_at, updated_at
         FROM games ORDER BY id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(LegacyGame {
                id: row.get(0)?,
                name: row.get(1)?,
                display_name: row.get(2)?,
                icon: row.get(3)?,
                install_path: row.get(4)?,
                active: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn read_users(connection: &Connection) -> Result<Vec<LegacyUser>, LibraryError> {
    let mut statement = connection.prepare("SELECT id, username FROM users ORDER BY id")?;
    {
        let rows = statement.query_map([], |row| {
            Ok(LegacyUser {
                id: row.get(0)?,
                username: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn read_oauths(connection: &Connection) -> Result<Vec<LegacyOauth>, LibraryError> {
    let mut statement = connection.prepare(
        "SELECT id, uid, oauth, platform, created_at, updated_at FROM oauths ORDER BY id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(LegacyOauth {
                id: row.get(0)?,
                user_id: row.get(1)?,
                token: row.get(2)?,
                platform: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn read_profiles(connection: &Connection) -> Result<Vec<LegacyProfile>, LibraryError> {
    let mut statement = connection.prepare(
        "SELECT id, game_id, user_id, name, display_name, is_active, description,
                last_used_at, created_at, updated_at FROM profiles ORDER BY id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(LegacyProfile {
                id: row.get(0)?,
                game_id: row.get(1)?,
                user_id: row.get(2)?,
                name: row.get(3)?,
                display_name: row.get(4)?,
                active: row.get(5)?,
                description: row.get(6)?,
                last_used_at: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn read_folders(connection: &Connection) -> Result<Vec<LegacyFolder>, LibraryError> {
    let mut statement = connection.prepare(
        "SELECT id, profile_id, parent_folder_id, name, folder_type, sort_order, is_expanded,
                created_at, updated_at FROM profile_folders ORDER BY id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(LegacyFolder {
                id: row.get(0)?,
                profile_id: row.get(1)?,
                parent_id: row.get(2)?,
                name: row.get(3)?,
                folder_type: row.get(4)?,
                sort_order: row.get(5)?,
                expanded: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn read_mods(connection: &Connection) -> Result<Vec<LegacyMod>, LibraryError> {
    let mut statement = connection.prepare(
        "SELECT m.mod_id, m.platform_id, m.game_id, m.name_id, m.display_name,
                m.original_name, m.url, m.source_type, m.tags, m.approval_status,
                m.depend_mod_id, COALESCE(v.current_version, '-'),
                COALESCE(v.available_versions, '[]'), COALESCE(d.download_url, ''),
                COALESCE(d.cache_path, ''), COALESCE(d.file_size, 0),
                COALESCE(d.download_status, ''), COALESCE(s.last_update_date, 0),
                COALESCE(s.online_update_date, 0), COALESCE(s.is_online_available, 1),
                COALESCE(s.is_local_not_found, 0), m.created_at, m.updated_at
         FROM mods m
         LEFT JOIN mod_versions v ON v.mod_id = m.mod_id
         LEFT JOIN mod_downloads d ON d.mod_id = m.mod_id
         LEFT JOIN mod_status s ON s.mod_id = m.mod_id
         ORDER BY m.mod_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, i64>(10)?,
            row.get::<_, String>(11)?,
            row.get::<_, String>(12)?,
            row.get::<_, String>(13)?,
            row.get::<_, String>(14)?,
            row.get::<_, u64>(15)?,
            row.get::<_, String>(16)?,
            row.get::<_, i64>(17)?,
            row.get::<_, i64>(18)?,
            row.get::<_, bool>(19)?,
            row.get::<_, bool>(20)?,
            row.get::<_, i64>(21)?,
            row.get::<_, i64>(22)?,
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        let (
            id,
            platform_id,
            game_id,
            name_id,
            display_name,
            original_name,
            url,
            source_type,
            tags_json,
            approval_status,
            dependency_id,
            current_version,
            available_versions_json,
            download_url,
            cache_path,
            file_size,
            download_status,
            last_update_at,
            online_update_at,
            online_available,
            local_missing,
            created_at,
            updated_at,
        ) = row?;
        result.push(LegacyMod {
            id,
            platform_id,
            game_id,
            name_id,
            display_name,
            original_name,
            url,
            source_type,
            tags: serde_json::from_str(&tags_json)?,
            approval_status,
            dependency_id,
            current_version,
            available_versions: serde_json::from_str(&available_versions_json)?,
            download_url,
            cache_path,
            file_size,
            download_status,
            last_update_at,
            online_update_at,
            online_available,
            local_missing,
            created_at,
            updated_at,
        });
    }
    Ok(result)
}

fn read_profile_mods(connection: &Connection) -> Result<Vec<LegacyProfileMod>, LibraryError> {
    let mut statement = connection.prepare(
        "SELECT id, profile_id, mod_id, parent_folder_id, sort_order, is_enabled, used_version,
                created_at, updated_at FROM profile_mods ORDER BY profile_id, sort_order, id",
    )?;
    {
        let rows = statement.query_map([], |row| {
            Ok(LegacyProfileMod {
                id: row.get(0)?,
                profile_id: row.get(1)?,
                mod_id: row.get(2)?,
                folder_id: row.get(3)?,
                sort_order: row.get(4)?,
                enabled: row.get(5)?,
                used_version: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn read_settings(connection: &Connection) -> Result<Vec<LegacySetting>, LibraryError> {
    let mut statement = connection
        .prepare("SELECT name, value, created_at, updated_at FROM settings ORDER BY name")?;
    {
        let rows = statement.query_map([], |row| {
            Ok(LegacySetting {
                name: row.get(0)?,
                value: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

fn stage_credentials(
    oauths: &[LegacyOauth],
    vault: &dyn CredentialVault,
) -> Result<Vec<StagedCredential>, LibraryError> {
    let mut grouped: BTreeMap<String, Vec<&LegacyOauth>> = BTreeMap::new();
    for oauth in oauths {
        grouped
            .entry(normalize_provider(&oauth.platform))
            .or_default()
            .push(oauth);
    }
    let mut staged: Vec<StagedCredential> = Vec::new();
    for (provider, rows) in grouped {
        let user_ids: HashSet<i64> = rows.iter().map(|row| row.user_id).collect();
        if user_ids.len() != 1 {
            cleanup_staged(vault, &staged)?;
            return Err(LibraryError::InvalidRecord(format!(
                "legacy provider {provider} belongs to multiple users"
            )));
        }
        let selected = rows
            .iter()
            .rev()
            .find(|row| !row.token.trim().is_empty())
            .copied()
            .unwrap_or(rows[rows.len() - 1]);
        let credential_ref = if selected.token.trim().is_empty() {
            None
        } else {
            match vault.store(LegacyCredential {
                owner_scope: OWNER_SCOPE,
                provider: &provider,
                secret: &selected.token,
            }) {
                Ok(reference) if !reference.trim().is_empty() => Some(reference),
                Ok(_) => {
                    cleanup_staged(vault, &staged)?;
                    return Err(LibraryError::CredentialVault {
                        provider,
                        message: "vault returned an empty reference".to_owned(),
                    });
                }
                Err(message) => {
                    cleanup_staged(vault, &staged)?;
                    return Err(LibraryError::CredentialVault { provider, message });
                }
            }
        };
        staged.push(StagedCredential {
            legacy_ids: rows.iter().map(|row| row.id).collect(),
            provider,
            credential_ref,
            created_at_ms: legacy_ms(selected.created_at),
            updated_at_ms: legacy_ms(selected.updated_at),
        });
    }
    Ok(staged)
}

fn cleanup_staged(
    vault: &dyn CredentialVault,
    staged: &[StagedCredential],
) -> Result<(), LibraryError> {
    let references = staged
        .iter()
        .filter_map(|credential| credential.credential_ref.clone())
        .collect::<Vec<_>>();
    cleanup_credentials(vault, &references)
}

fn cleanup_credentials(
    vault: &dyn CredentialVault,
    references: &[String],
) -> Result<(), LibraryError> {
    for credential_ref in references.iter().rev() {
        vault
            .delete(credential_ref)
            .map_err(|message| LibraryError::CredentialCleanup {
                credential_ref: credential_ref.clone(),
                message,
            })?;
    }
    Ok(())
}

fn build_snapshot(
    dump: &LegacyDump,
    source_digest: &str,
    staged_credentials: Vec<StagedCredential>,
) -> Result<(LibrarySnapshot, Vec<String>), LibraryError> {
    let mut snapshot = LibrarySnapshot::default();
    let mut warnings = Vec::new();
    let mut game_ids = HashMap::new();
    let mut profile_ids = HashMap::new();
    let mut folder_ids = HashMap::new();
    let mut mod_ids = HashMap::new();

    for user in &dump.users {
        add_mapping(
            &mut snapshot,
            source_digest,
            "user",
            user.id,
            OWNER_SCOPE.to_owned(),
        );
    }

    for game in &dump.games {
        let game_key = if game.name.trim().eq_ignore_ascii_case("rc") { "rogue-core" } else { game.name.as_str() };
        let segment = stable_segment(game_key, game.id);
        let game_id = format!("game:{segment}");
        let installation_id = format!("installation:legacy:{}", game.id);
        game_ids.insert(game.id, game_id.clone());
        snapshot.games.push(GameDefinition {
            game_id: game_id.clone(),
            game_key: game_key.to_owned(),
            display_name: game.display_name.clone(),
            icon: game.icon.clone(),
            active: game.active,
            created_at_ms: legacy_ms(game.created_at),
            updated_at_ms: legacy_ms(game.updated_at),
        });
        snapshot.installations.push(GameInstallation {
            installation_id: installation_id.clone(),
            game_id: game_id.clone(),
            root_path: nonempty(&game.install_path),
            state: if game.install_path.trim().is_empty() {
                warnings.push(format!(
                    "game {} has no configured installation path",
                    game.name
                ));
                "unresolved".to_owned()
            } else {
                "configured".to_owned()
            },
            incarnation_id: format!("incarnation:installation:legacy:{}", game.id),
            created_at_ms: legacy_ms(game.created_at),
            updated_at_ms: legacy_ms(game.updated_at),
        });
        add_mapping(&mut snapshot, source_digest, "game", game.id, game_id);
        add_mapping(
            &mut snapshot,
            source_digest,
            "installation",
            game.id,
            installation_id,
        );
    }

    let user_ids: HashSet<i64> = dump.users.iter().map(|user| user.id).collect();
    for profile in &dump.profiles {
        if !user_ids.contains(&profile.user_id) {
            return Err(missing("user", profile.user_id));
        }
        let game_id = mapped(&game_ids, "game", profile.game_id)?;
        let profile_id = format!("profile:legacy:{}", profile.id);
        profile_ids.insert(profile.id, profile_id.clone());
        snapshot.profiles.push(ProfileRecord {
            profile_id: profile_id.clone(),
            game_id,
            owner_scope: OWNER_SCOPE.to_owned(),
            name: profile.name.clone(),
            display_name: profile.display_name.clone(),
            active: profile.active,
            description: profile.description.clone(),
            revision: 1,
            content_revision: 1,
            lease_epoch: 0,
            incarnation_id: format!("incarnation:profile:legacy:{}", profile.id),
            last_used_at_ms: legacy_ms(profile.last_used_at),
            created_at_ms: legacy_ms(profile.created_at),
            updated_at_ms: legacy_ms(profile.updated_at),
            deleted_at_ms: None,
        });
        add_mapping(
            &mut snapshot,
            source_digest,
            "profile",
            profile.id,
            profile_id,
        );
    }

    for folder in &dump.folders {
        let profile_id = mapped(&profile_ids, "profile", folder.profile_id)?;
        let group_id = format!("group:legacy:{}", folder.id);
        folder_ids.insert(folder.id, group_id.clone());
        snapshot.groups.push(GroupRecord {
            group_id: group_id.clone(),
            profile_id,
            parent_group_id: None,
            name: folder.name.clone(),
            group_type: folder.folder_type.clone(),
            sort_order: folder.sort_order,
            expanded: folder.expanded,
            created_at_ms: legacy_ms(folder.created_at),
            updated_at_ms: legacy_ms(folder.updated_at),
        });
        add_mapping(&mut snapshot, source_digest, "group", folder.id, group_id);
    }
    for (record, legacy) in snapshot.groups.iter_mut().zip(&dump.folders) {
        record.parent_group_id = legacy
            .parent_id
            .map(|id| mapped(&folder_ids, "group", id))
            .transpose()?;
    }

    for legacy_mod in &dump.mods {
        let game_id = mapped(&game_ids, "game", legacy_mod.game_id)?;
        let game_key = dump
            .games
            .iter()
            .find(|game| game.id == legacy_mod.game_id)
            .map(|game| game.name.clone())
            .ok_or_else(|| missing("game", legacy_mod.game_id))?;
        let mod_id = format!("mod:legacy:{}", legacy_mod.id);
        mod_ids.insert(legacy_mod.id, mod_id.clone());
        snapshot.mods.push(LibraryMod {
            mod_id: mod_id.clone(),
            game_id,
            source_id: normalize_provider(&legacy_mod.source_type),
            remote_game_key: game_key,
            remote_mod_id: if legacy_mod.platform_id > 0 {
                legacy_mod.platform_id.to_string()
            } else {
                format!("legacy-local:{}", legacy_mod.id)
            },
            revision: 1,
            incarnation_id: format!("incarnation:mod:legacy:{}", legacy_mod.id),
            name_id: legacy_mod.name_id.clone(),
            display_name: legacy_mod.display_name.clone(),
            original_name: legacy_mod.original_name.clone(),
            url: legacy_mod.url.clone(),
            tags: legacy_mod.tags.clone(),
            approval_status: legacy_mod.approval_status.clone(),
            dependency_mod_id: None,
            last_update_at_ms: legacy_ms(legacy_mod.last_update_at),
            online_update_at_ms: legacy_ms(legacy_mod.online_update_at),
            online_available: legacy_mod.online_available,
            local_missing: legacy_mod.local_missing,
            created_at_ms: legacy_ms(legacy_mod.created_at),
            updated_at_ms: legacy_ms(legacy_mod.updated_at),
        });
        add_mapping(&mut snapshot, source_digest, "mod", legacy_mod.id, mod_id);
    }
    for (record, legacy) in snapshot.mods.iter_mut().zip(&dump.mods) {
        if legacy.dependency_id > 0 {
            record.dependency_mod_id = Some(mapped(&mod_ids, "mod", legacy.dependency_id)?);
        }
    }

    let mut used_versions: HashMap<i64, Vec<String>> = HashMap::new();
    for membership in &dump.profile_mods {
        if !membership.used_version.trim().is_empty() {
            used_versions
                .entry(membership.mod_id)
                .or_default()
                .push(membership.used_version.clone());
        }
    }
    for legacy_mod in &dump.mods {
        let mod_id = mapped(&mod_ids, "mod", legacy_mod.id)?;
        let mut versions = Vec::new();
        if !legacy_mod.current_version.trim().is_empty() && legacy_mod.current_version != "-" {
            versions.push(legacy_mod.current_version.clone());
        }
        versions.extend(legacy_mod.available_versions.iter().cloned());
        versions.extend(used_versions.remove(&legacy_mod.id).unwrap_or_default());
        if versions.is_empty() && !legacy_mod.cache_path.trim().is_empty() {
            versions.push("legacy-unversioned".to_owned());
        }
        let mut seen = HashSet::new();
        for version in versions {
            if version.trim().is_empty() || !seen.insert(version.clone()) {
                continue;
            }
            let current = version == legacy_mod.current_version
                || (legacy_mod.current_version == "-" && version == "legacy-unversioned");
            let has_location = current && !legacy_mod.cache_path.trim().is_empty();
            snapshot.versions.push(ModContentVersion {
                mod_id: mod_id.clone(),
                version_id: version,
                remote_file_id: None,
                artifact_digest: None,
                legacy_location: has_location.then(|| legacy_mod.cache_path.clone()),
                source_url: current
                    .then(|| legacy_mod.download_url.clone())
                    .filter(|url| !url.is_empty()),
                file_size: if current { legacy_mod.file_size } else { 0 },
                legacy_download_status: current
                    .then(|| legacy_mod.download_status.clone())
                    .filter(|status| !status.is_empty()),
                availability: if has_location {
                    "legacy-unverified".to_owned()
                } else if !legacy_mod.download_url.trim().is_empty() {
                    "remote".to_owned()
                } else {
                    "metadata-only".to_owned()
                },
                current,
                created_at_ms: legacy_ms(legacy_mod.created_at),
                updated_at_ms: legacy_ms(legacy_mod.updated_at),
            });
        }
        if !legacy_mod.cache_path.trim().is_empty() {
            warnings.push(format!(
                "legacy cache path for mod {} is unverified and is not an artifact or receipt",
                legacy_mod.id
            ));
        }
    }

    for membership in &dump.profile_mods {
        let profile_id = mapped(&profile_ids, "profile", membership.profile_id)?;
        let mod_id = mapped(&mod_ids, "mod", membership.mod_id)?;
        let profile_mod_id = format!("profile-mod:legacy:{}", membership.id);
        snapshot.profile_mods.push(ProfileModRecord {
            profile_mod_id: profile_mod_id.clone(),
            profile_id,
            mod_id,
            group_id: membership
                .folder_id
                .map(|id| mapped(&folder_ids, "group", id))
                .transpose()?,
            incarnation_id: format!("incarnation:profile-mod:legacy:{}", membership.id),
            sort_order: membership.sort_order,
            enabled: membership.enabled,
            used_version: nonempty(&membership.used_version),
            version_selection_kind: if membership.used_version.trim().is_empty() {
                "auto".to_owned()
            } else {
                "pinned".to_owned()
            },
            selected_version_id: nonempty(&membership.used_version),
            created_at_ms: legacy_ms(membership.created_at),
            updated_at_ms: legacy_ms(membership.updated_at),
        });
        add_mapping(
            &mut snapshot,
            source_digest,
            "profile-mod",
            membership.id,
            profile_mod_id,
        );
    }

    for setting in &dump.settings {
        if let Some((legacy_game_id, setting_name)) = parse_game_setting(&setting.name) {
            let game_id = mapped(&game_ids, "game", legacy_game_id)?;
            if setting_name == "installHash" {
                snapshot.legacy_install_hints.push(LegacyInstallHint {
                    game_id,
                    installed_hash: setting.value.clone(),
                    claim_status: "unverified".to_owned(),
                });
                warnings.push(format!(
                    "legacy installedHash for game {legacy_game_id} was retained only as an unverified hint"
                ));
            } else {
                snapshot.settings.push(SettingRecord {
                    scope_id: game_id,
                    name: setting_name.to_owned(),
                    value: setting.value.clone(),
                    created_at_ms: legacy_ms(setting.created_at),
                    updated_at_ms: legacy_ms(setting.updated_at),
                });
            }
        } else {
            snapshot.settings.push(SettingRecord {
                scope_id: GLOBAL_SETTING_SCOPE.to_owned(),
                name: setting.name.clone(),
                value: setting.value.clone(),
                created_at_ms: legacy_ms(setting.created_at),
                updated_at_ms: legacy_ms(setting.updated_at),
            });
        }
    }

    for credential in staged_credentials {
        let credential_id = format!(
            "credential:legacy:{}",
            stable_segment(&credential.provider, 0)
        );
        for legacy_id in credential.legacy_ids {
            add_mapping(
                &mut snapshot,
                source_digest,
                "credential",
                legacy_id,
                credential_id.clone(),
            );
        }
        snapshot.credentials.push(CredentialRecord {
            credential_id,
            owner_scope: OWNER_SCOPE.to_owned(),
            provider: credential.provider,
            credential_ref: credential.credential_ref,
            created_at_ms: credential.created_at_ms,
            updated_at_ms: credential.updated_at_ms,
        });
    }

    Ok((snapshot, warnings))
}

fn add_mapping(
    snapshot: &mut LibrarySnapshot,
    source_digest: &str,
    entity_kind: &str,
    legacy_id: i64,
    new_id: String,
) {
    snapshot.legacy_mappings.push(LegacyIdMapping {
        source_digest: source_digest.to_owned(),
        entity_kind: entity_kind.to_owned(),
        legacy_id: legacy_id.to_string(),
        new_id,
    });
}

fn mapped(
    mappings: &HashMap<i64, String>,
    entity: &'static str,
    legacy_id: i64,
) -> Result<String, LibraryError> {
    mappings
        .get(&legacy_id)
        .cloned()
        .ok_or_else(|| missing(entity, legacy_id))
}

fn missing(entity: &'static str, legacy_id: i64) -> LibraryError {
    LibraryError::MissingLegacyReference { entity, legacy_id }
}

fn normalize_provider(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "mod.io" | "modio" => "modio".to_owned(),
        "nexus" | "nexusmods" | "nexus mods" => "nexusmods".to_owned(),
        "modcat" | "mintcat" => value.trim().to_ascii_lowercase(),
        "local" => "local".to_owned(),
        "" | "unknown" => "legacy-unknown".to_owned(),
        other => stable_segment(other, 0),
    }
}

fn stable_segment(value: &str, fallback: i64) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_dash = false;
    for character in value.trim().chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_alphanumeric() {
            normalized.push(character);
            previous_dash = false;
        } else if !previous_dash && !normalized.is_empty() {
            normalized.push('-');
            previous_dash = true;
        }
    }
    while normalized.ends_with('-') {
        normalized.pop();
    }
    if normalized.is_empty() {
        format!("legacy-{fallback}")
    } else {
        normalized
    }
}

fn parse_game_setting(name: &str) -> Option<(i64, &str)> {
    let remainder = name.strip_prefix("game_")?;
    let (id, setting) = remainder.split_once('_')?;
    Some((id.parse().ok()?, setting))
}

fn nonempty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_owned())
}

fn legacy_ms(seconds: i64) -> i64 {
    seconds.saturating_mul(1000)
}

fn current_time_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}
