use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use mintcat_sdk::{
    GameSummary, GamesListParams, GamesListResult, GroupSummary, GroupsCreateParams,
    GroupsDeleteParams, GroupsListParams, GroupsListResult, GroupsMoveParams, GroupsUpdateParams,
    InstallationSummary, LibraryMutationResult, ModDetails, ModEntityResult, ModRef, ModSummary,
    ModVersionSummary, ModsCleanupMissingParams, ModsGetParams, ModsListParams, ModsListResult,
    ModsMoveParams, ModsRemoveParams, ModsRenameParams, ModsSetEnabledBatchParams,
    ModsSetEnabledParams, ProfileSummary, ProfilesActivateParams, ProfilesCloneParams,
    ProfilesCreateParams, ProfilesDeleteParams, ProfilesListParams, ProfilesListResult,
    ProfilesUpdateParams, VersionSelection, VersionSelectionKind,
};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    LibraryAuthorization, LibraryError, OwnerStore, CONTENT_WRITE_PERMISSION,
    METADATA_WRITE_PERMISSION, PROFILE_WRITE_PERMISSION, READ_PERMISSION,
};

/// Trusted-host enrollment envelope. The descriptor comes from an authenticated
/// signed package; installation is the active native game's validation result,
/// never caller-supplied Application JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameEnrollmentRequest {
    pub signed_descriptor: mintcat_game_api::GameDescriptor,
    pub installation: Option<mintcat_game_api::ValidatedGameInstallation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GameEnrollmentResult {
    pub game_id: String,
    pub definition_revision: u64,
    pub installation: Option<crate::GameInstallation>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnrolledDefinition {
    descriptor: mintcat_game_api::GameDescriptor,
    revision: u64,
}

impl OwnerStore {
    pub fn register_game(
        &self,
        authorization: &LibraryAuthorization,
        idempotency_key: &str,
        request: &GameEnrollmentRequest,
    ) -> Result<GameEnrollmentResult, LibraryError> {
        authorization.require(METADATA_WRITE_PERMISSION)?;
        let descriptor = &request.signed_descriptor;
        descriptor.validate().map_err(|e| LibraryError::InvalidRecord(e.to_string()))?;
        let game_key = descriptor.game_id.strip_prefix("game:")
            .filter(|key| !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
            .ok_or_else(|| LibraryError::InvalidRecord("signed game identity must be canonical game:<key>".into()))?;
        if let Some(installation) = &request.installation {
            validate_enrollment_installation(descriptor, installation)?;
        }
        let inputs_hash = hash_json(&serde_json::to_value(request)?)?;
        let now = now_ms()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(result) = replay_mutation(&transaction, authorization, "games.register", idempotency_key, &inputs_hash)? {
            transaction.commit()?;
            return Ok(result);
        }
        // A legacy alias must not create a second game identity. Ambiguous or
        // noncanonical aliases require an explicit migration, not reassignment
        // of existing profiles or installation incarnations.
        let mut aliases = transaction.prepare(
            "SELECT game_id, game_key, display_name FROM library_games WHERE game_id <> ?1",
        )?;
        let rows = aliases.query_map([&descriptor.game_id], |r| Ok((
            r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
        )))?;
        for row in rows {
            let (id, key, display_name) = row?;
            if key.eq_ignore_ascii_case(game_key)
                || display_name.eq_ignore_ascii_case(&descriptor.display_name)
                || descriptor.source_bindings.iter().any(|source| source.remote_game_key.eq_ignore_ascii_case(&key))
            {
                return Err(LibraryError::InvalidRecord(format!("signed game alias collides with existing identity {id}")));
            }
        }
        drop(aliases);
        let definition_scope = format!("enrollment:game:{}", descriptor.game_id);
        let prior: Option<String> = transaction.query_row(
            "SELECT value FROM library_settings WHERE scope_id = ?1 AND name = 'definition'",
            [&definition_scope], |r| r.get(0),
        ).optional()?;
        let prior: Option<EnrolledDefinition> = prior.map(|v| serde_json::from_str(&v)).transpose()?;
        let revision = match &prior {
            Some(prior) if prior.descriptor == *descriptor => prior.revision,
            Some(prior) => prior.revision.checked_add(1).ok_or_else(|| LibraryError::InvalidRecord("game definition revision overflow".into()))?,
            None => 1,
        };
        transaction.execute(
            "INSERT INTO library_games(game_id, game_key, display_name, icon, active, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, '', 0, ?4, ?4)
             ON CONFLICT(game_id) DO UPDATE SET display_name = excluded.display_name,
               updated_at_ms = CASE WHEN library_games.display_name <> excluded.display_name THEN excluded.updated_at_ms ELSE library_games.updated_at_ms END",
            params![descriptor.game_id, game_key, descriptor.display_name, now],
        )?;
        if prior.as_ref().is_none_or(|prior| prior.descriptor != *descriptor) {
            transaction.execute(
                "INSERT INTO library_settings(scope_id, name, value, created_at_ms, updated_at_ms)
                 VALUES (?1, 'definition', ?2, ?3, ?3)
                 ON CONFLICT(scope_id, name) DO UPDATE SET value = excluded.value, updated_at_ms = excluded.updated_at_ms",
                params![definition_scope, serde_json::to_string(&EnrolledDefinition { descriptor: descriptor.clone(), revision })?, now],
            )?;
        }
        let installation = request.installation.as_ref().map(|validated| {
            enroll_installation(&transaction, validated, now)
        }).transpose()?;
        let result = GameEnrollmentResult { game_id: descriptor.game_id.clone(), definition_revision: revision, installation };
        let operation_id = Uuid::new_v4().to_string();
        record_mutation(&transaction, authorization, "games.register", idempotency_key, &inputs_hash, &operation_id, &result, now)?;
        insert_application_event(&transaction, "game.registered", &json!({"operationId":operation_id,"result":result}), now)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn registered_installation(
        &self,
        authorization: &LibraryAuthorization,
        installation_id: &str,
    ) -> Result<mintcat_game_api::ValidatedGameInstallation, LibraryError> {
        authorization.require(READ_PERMISSION)?;
        let connection = self.connection()?;
        let encoded: Option<String> = connection.query_row(
            "SELECT s.value FROM library_settings s JOIN library_installations i
             ON s.scope_id = 'enrollment:installation:' || i.installation_id
             WHERE i.installation_id = ?1 AND s.name = 'validated'",
            [installation_id], |r| r.get(0),
        ).optional()?;
        serde_json::from_str(&encoded.ok_or_else(|| LibraryError::NotFound(format!("validated installation {installation_id}")))?).map_err(Into::into)
    }
}

fn validate_enrollment_installation(
    descriptor: &mintcat_game_api::GameDescriptor,
    installation: &mintcat_game_api::ValidatedGameInstallation,
) -> Result<(), LibraryError> {
    let variant = descriptor.variants.iter().find(|v| v.variant_id == installation.variant_id)
        .ok_or_else(|| LibraryError::InvalidRecord("validated installation variant is absent from signed descriptor".into()))?;
    if installation.game_id != descriptor.game_id
        || installation.main_pak_relative_path != variant.main_pak_relative_path
        || installation.target != variant.target
        || installation.main_pak_size == 0
        || installation.fingerprint.strip_prefix("sha256:").is_none_or(|digest| {
            digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err(LibraryError::InvalidRecord("validated installation differs from signed game identity/layout".into()));
    }
    let root = std::path::Path::new(&installation.root_path);
    if !root.is_absolute() || !root.is_dir() || root.canonicalize()? != root {
        return Err(LibraryError::InvalidRecord("validated installation root must be an absolute canonical directory".into()));
    }
    let pak = root.join(&installation.main_pak_relative_path);
    let metadata = std::fs::symlink_metadata(&pak)?;
    if !metadata.is_file() || metadata.file_type().is_symlink()
        || metadata.len() != installation.main_pak_size
        || !pak.canonicalize()?.starts_with(root)
    {
        return Err(LibraryError::InvalidRecord("validated main PAK metadata changed".into()));
    }
    Ok(())
}

fn enroll_installation(
    transaction: &Transaction<'_>,
    validated: &mintcat_game_api::ValidatedGameInstallation,
    now: i64,
) -> Result<crate::GameInstallation, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT installation_id, game_id, root_path, state, incarnation_id, created_at_ms, updated_at_ms
         FROM library_installations WHERE root_path IS NOT NULL ORDER BY installation_id",
    )?;
    let rows = statement.query_map([], |r| Ok(crate::GameInstallation {
        installation_id:r.get(0)?, game_id:r.get(1)?, root_path:r.get(2)?, state:r.get(3)?,
        incarnation_id:r.get(4)?, created_at_ms:r.get(5)?, updated_at_ms:r.get(6)?,
    }))?;
    let mut existing = None;
    for row in rows {
        let installation = row?;
        let root = std::path::Path::new(installation.root_path.as_deref().unwrap_or(""));
        let retained: Option<String> = transaction.query_row(
            "SELECT value FROM library_settings WHERE scope_id = ?1 AND name = 'validated'",
            [format!("enrollment:installation:{}", installation.installation_id)], |r| r.get(0),
        ).optional()?;
        let retained: Option<mintcat_game_api::ValidatedGameInstallation> =
            retained.map(|value| serde_json::from_str(&value)).transpose()?;
        let prior_pak = retained.as_ref().map(|value| value.main_pak_relative_path.as_str())
            .unwrap_or(&validated.main_pak_relative_path);
        let collision = root == std::path::Path::new(&validated.root_path)
            || root.canonicalize().is_ok_and(|path| path == std::path::Path::new(&validated.root_path))
            || same_physical_file(&root.join(prior_pak), &std::path::Path::new(&validated.root_path).join(&validated.main_pak_relative_path));
        if collision {
            if existing.is_some() || installation.game_id != validated.game_id {
                return Err(LibraryError::InvalidRecord("installation physical identity collides with existing enrollment".into()));
            }
            existing = Some(installation);
        }
    }
    drop(statement);
    let installation = if let Some(existing) = existing {
        let stored: Option<String> = transaction.query_row(
            "SELECT value FROM library_settings WHERE scope_id = ?1 AND name = 'validated'",
            [format!("enrollment:installation:{}", existing.installation_id)], |r| r.get(0),
        ).optional()?;
        if let Some(stored) = stored {
            let prior: mintcat_game_api::ValidatedGameInstallation = serde_json::from_str(&stored)?;
            if prior != *validated {
                return Err(LibraryError::StaleTarget("existing installation variant or fingerprint differs; explicit migration required".into()));
            }
        }
        if existing.root_path.as_deref() != Some(validated.root_path.as_str()) || existing.state != "configured" {
            return Err(LibraryError::InvalidRecord("legacy installation alias requires explicit migration".into()));
        }
        existing
    } else {
        let installation = crate::GameInstallation {
            installation_id:format!("installation:{}", Uuid::new_v4()), game_id:validated.game_id.clone(),
            root_path:Some(validated.root_path.clone()), state:"configured".into(),
            incarnation_id:format!("incarnation:installation:{}", Uuid::new_v4()), created_at_ms:now, updated_at_ms:now,
        };
        transaction.execute(
            "INSERT INTO library_installations(installation_id, game_id, root_path, state, incarnation_id, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, 'configured', ?4, ?5, ?5)",
            params![installation.installation_id, installation.game_id, validated.root_path, installation.incarnation_id, now],
        )?;
        installation
    };
    transaction.execute(
        "INSERT INTO library_settings(scope_id, name, value, created_at_ms, updated_at_ms)
         VALUES (?1, 'validated', ?2, ?3, ?3) ON CONFLICT(scope_id, name) DO NOTHING",
        params![format!("enrollment:installation:{}", installation.installation_id), serde_json::to_string(validated)?, now],
    )?;
    Ok(installation)
}

fn same_physical_file(left: &std::path::Path, right: &std::path::Path) -> bool {
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(left), Ok(right)) = (left.metadata(), right.metadata()) {
            return left.dev() == right.dev() && left.ino() == right.ino();
        }
    }
    left.canonicalize().ok().zip(right.canonicalize().ok()).is_some_and(|(left, right)| left == right)
}

#[cfg(test)]
mod enrollment_tests {
    use super::*;
    use std::path::Path;
    use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};

    fn authority() -> LibraryAuthorization {
        LibraryAuthorization::new("human", [READ_PERMISSION, METADATA_WRITE_PERMISSION, PROFILE_WRITE_PERMISSION])
    }

    fn descriptor() -> mintcat_game_api::GameDescriptor {
        serde_json::from_value(json!({
            "schemaVersion":1,"gameId":"game:fixture","displayName":"Fixture Game",
            "adapterVersion":"1","settingsSchemaVersion":1,"settingsSchema":{},
            "variants":[{"variantId":"steam","displayName":"Steam","store":"steam",
                "storeAppId":null,"target":"fixture-target","mainPakRelativePath":"Content/main.pak",
                "paksRelativePath":"Content","binariesRelativePath":"Binaries",
                "modPakRelativePath":"Content/mod.pak","processNames":["Fixture.exe"]}],
            "inputFormats":["pak"],"sourceBindings":[],"resources":[],"capabilities":[]
        })).unwrap()
    }

    fn request(root: &Path) -> GameEnrollmentRequest {
        std::fs::create_dir_all(root.join("Content")).unwrap();
        std::fs::write(root.join("Content/main.pak"), b"fixture").unwrap();
        GameEnrollmentRequest {
            signed_descriptor:descriptor(),
            installation:Some(mintcat_game_api::ValidatedGameInstallation {
                game_id:"game:fixture".into(), variant_id:"steam".into(),
                root_path:root.canonicalize().unwrap().to_str().unwrap().into(),
                main_pak_relative_path:"Content/main.pak".into(), fingerprint:format!("sha256:{}", "a".repeat(64)),
                target:"fixture-target".into(), main_pak_size:7,
            }),
        }
    }

    fn store(root: &Path) -> crate::LibraryStore {
        let layout = RuntimePaths::new(root.join("state")).unwrap().initialize(RuntimeEnvironment::Test).unwrap();
        crate::LibraryStore::open(&layout).unwrap()
    }

    #[test]
    fn fresh_enrollment_replays_identity_and_catalog_sync_preserves_profile_and_installation() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = store(sandbox.path());
        let authorization = authority();
        let request = request(&sandbox.path().join("game"));
        let enrolled = store.register_game(&authorization, "register", &request).unwrap();
        let installation = enrolled.installation.clone().unwrap();
        assert_eq!(store.register_game(&authorization, "register", &request).unwrap(), enrolled);
        assert_eq!(store.register_game(&authorization, "second-key", &request).unwrap().installation, Some(installation.clone()));
        let games = store.list_games(&authorization, &serde_json::from_value(json!({"pageSize":100})).unwrap()).unwrap();
        assert_eq!(games.items[0].installations[0].installation_id, installation.installation_id);
        assert_eq!(store.registered_installation(&authorization, &installation.installation_id).unwrap(), request.installation.clone().unwrap());
        let profile = store.create_profile(&authorization, "profile", &serde_json::from_value(json!({
            "gameId":enrolled.game_id,"name":"Fresh Profile","active":true
        })).unwrap()).unwrap();
        let before = store.snapshot().unwrap();
        let mut definition_only = request.clone();
        definition_only.installation = None;
        definition_only.signed_descriptor.display_name = "Updated Fixture".into();
        let synchronized = store.register_game(&authorization, "sync", &definition_only).unwrap();
        assert_eq!(synchronized.definition_revision, 2);
        let after = store.snapshot().unwrap();
        assert_eq!(after.profiles, before.profiles);
        assert_eq!(after.installations, before.installations);
        assert_eq!(after.profiles[0].incarnation_id, profile.incarnation_id);
        assert!(matches!(store.register_game(&authorization, "register", &definition_only), Err(LibraryError::IdempotencyConflict)));
    }

    #[test]
    fn enrollment_refuses_cross_game_aliases_invalid_metadata_and_changed_physical_identity() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = store(sandbox.path());
        let authorization = authority();
        let request = request(&sandbox.path().join("game"));
        let enrolled = store.register_game(&authorization, "register", &request).unwrap();
        let mut cross_game = request.clone();
        cross_game.installation.as_mut().unwrap().game_id = "game:other".into();
        assert!(matches!(store.register_game(&authorization, "cross-game", &cross_game), Err(LibraryError::InvalidRecord(_))));
        let mut alias = request.clone();
        alias.signed_descriptor.game_id = "game:alias".into();
        alias.installation = None;
        assert!(matches!(store.register_game(&authorization, "alias", &alias), Err(LibraryError::InvalidRecord(_))));
        let mut invalid = request.clone();
        invalid.installation.as_mut().unwrap().fingerprint = "unverified".into();
        assert!(matches!(store.register_game(&authorization, "invalid", &invalid), Err(LibraryError::InvalidRecord(_))));
        let mut changed = request.clone();
        changed.installation.as_mut().unwrap().fingerprint = format!("sha256:{}", "b".repeat(64));
        assert!(matches!(store.register_game(&authorization, "changed", &changed), Err(LibraryError::StaleTarget(_))));
        let unauthorized = LibraryAuthorization::new("agent", [READ_PERMISSION]);
        assert!(matches!(store.register_game(&unauthorized, "denied", &request), Err(LibraryError::PermissionDenied(_))));
        let snapshot = store.snapshot().unwrap();
        assert_eq!(snapshot.games.iter().map(|g| g.game_id.as_str()).collect::<Vec<_>>(), ["game:fixture"]);
        assert_eq!(snapshot.installations, vec![enrolled.installation.unwrap()]);
    }

    #[cfg(unix)]
    #[test]
    fn enrollment_rejects_hardlinked_pak_under_a_second_game_root() {
        let sandbox = tempfile::tempdir().unwrap();
        let store = store(sandbox.path());
        let authorization = authority();
        let first = request(&sandbox.path().join("first"));
        store.register_game(&authorization, "first", &first).unwrap();
        let mut second = request(&sandbox.path().join("second"));
        let source = Path::new(&first.installation.as_ref().unwrap().root_path).join("Content/main.pak");
        let target = Path::new(&second.installation.as_ref().unwrap().root_path).join("Content/main.pak");
        std::fs::remove_file(&target).unwrap();
        std::fs::hard_link(source, target).unwrap();
        second.signed_descriptor.game_id = "game:second".into();
        second.signed_descriptor.display_name = "Second Game".into();
        second.installation.as_mut().unwrap().game_id = "game:second".into();
        assert!(matches!(store.register_game(&authorization, "second", &second), Err(LibraryError::InvalidRecord(_))));
        let games = store.list_games(&authorization, &serde_json::from_value(json!({"pageSize":100})).unwrap()).unwrap();
        assert_eq!(games.items.iter().map(|g| g.game_id.as_str()).collect::<Vec<_>>(), ["game:fixture"]);
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageCursor {
    kind: String,
    scope_hash: String,
    snapshot: String,
    offset: u64,
}

struct SetModsEnabled<'a> {
    scope: &'static str,
    profile_id: &'a str,
    profile_mod_ids: &'a [String],
    expected_content_revision: u64,
    enabled: bool,
}

#[derive(Debug)]
pub(crate) struct ProfileRow {
    pub(crate) profile_id: String,
    pub(crate) game_id: String,
    pub(crate) owner_scope: String,
    pub(crate) name: String,
    pub(crate) display_name: String,
    pub(crate) active: bool,
    pub(crate) description: String,
    pub(crate) revision: u64,
    pub(crate) content_revision: u64,
    pub(crate) incarnation_id: String,
}

impl ProfileRow {
    fn summary(&self) -> ProfileSummary {
        ProfileSummary {
            profile_id: self.profile_id.clone(),
            game_id: self.game_id.clone(),
            name: self.name.clone(),
            display_name: self.display_name.clone(),
            active: self.active,
            description: self.description.clone(),
            revision: self.revision,
            content_revision: self.content_revision,
            incarnation_id: self.incarnation_id.clone(),
        }
    }
}

impl OwnerStore { pub fn list_games(
    &self,
    authorization: &LibraryAuthorization,
    request: &GamesListParams,
) -> Result<GamesListResult, LibraryError> {
    authorization.require(READ_PERMISSION)?;
    validate_page_size(request.page_size)?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let scope_hash = hash_json(&json!({"gameId": request.game_id}))?;
    let snapshot = games_snapshot_fingerprint(&transaction, request.game_id.as_deref())?;
    let offset = cursor_offset(
        request.cursor.as_deref(),
        "games.list",
        &scope_hash,
        &snapshot,
    )?;
    let mut statement = transaction.prepare(
        "SELECT game_id, game_key, display_name, active
         FROM library_games
         WHERE (?1 IS NULL OR game_id = ?1)
         ORDER BY game_key COLLATE NOCASE, game_id
         LIMIT ?2 OFFSET ?3",
    )?;
    let rows = statement.query_map(
        params![request.game_id, request.page_size + 1, offset],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        },
    )?;
    let games = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let has_more = games.len() as u64 > request.page_size;
    let mut items = Vec::with_capacity(games.len().min(request.page_size as usize));
    for (game_id, game_key, display_name, active) in
        games.into_iter().take(request.page_size as usize)
    {
        let mut installations = transaction.prepare(
            "SELECT installation_id, root_path, state, incarnation_id
             FROM library_installations WHERE game_id = ?1
             ORDER BY installation_id",
        )?;
        let rows = installations.query_map(params![game_id], |row| {
            Ok(InstallationSummary {
                installation_id: row.get(0)?,
                root_path: row.get(1)?,
                state: row.get(2)?,
                incarnation_id: row.get(3)?,
            })
        })?;
        items.push(GameSummary {
            game_id,
            game_key,
            display_name,
            active,
            installations: rows.collect::<Result<_, _>>()?,
        });
    }
    let next_cursor = has_more
        .then(|| {
            encode_cursor(
                "games.list",
                &scope_hash,
                &snapshot,
                offset + request.page_size,
            )
        })
        .transpose()?;
    transaction.commit()?;
    Ok(GamesListResult { items, next_cursor })
}

pub fn list_profiles(
    &self,
    authorization: &LibraryAuthorization,
    request: &ProfilesListParams,
) -> Result<ProfilesListResult, LibraryError> {
    authorization.require(READ_PERMISSION)?;
    validate_page_size(request.page_size)?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    ensure_game_exists(&transaction, &request.game_id)?;
    let scope_hash = hash_json(&json!({
        "ownerScope": authorization.owner_scope(),
        "gameId": request.game_id,
    }))?;
    let snapshot = profiles_snapshot_fingerprint(
        &transaction,
        authorization.owner_scope(),
        &request.game_id,
    )?;
    let offset = cursor_offset(
        request.cursor.as_deref(),
        "profiles.list",
        &scope_hash,
        &snapshot,
    )?;
    let mut statement = transaction.prepare(
        "SELECT profile_id, game_id, owner_scope, name, display_name, active,
                description, revision, content_revision, incarnation_id
         FROM library_profiles
         WHERE owner_scope = ?1 AND game_id = ?2 AND deleted_at_ms IS NULL
         ORDER BY name COLLATE NOCASE, profile_id
         LIMIT ?3 OFFSET ?4",
    )?;
    let rows = statement.query_map(
        params![
            authorization.owner_scope(),
            request.game_id,
            request.page_size + 1,
            offset
        ],
        map_profile_row,
    )?;
    let mut profiles = rows.collect::<Result<Vec<_>, _>>()?;
    let has_more = profiles.len() as u64 > request.page_size;
    profiles.truncate(request.page_size as usize);
    let items = profiles.iter().map(ProfileRow::summary).collect();
    let next_cursor = has_more
        .then(|| {
            encode_cursor(
                "profiles.list",
                &scope_hash,
                &snapshot,
                offset + request.page_size,
            )
        })
        .transpose()?;
    drop(statement);
    transaction.commit()?;
    Ok(ProfilesListResult { items, next_cursor })
}

pub fn create_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesCreateParams,
) -> Result<ProfileSummary, LibraryError> {
    authorization.require(PROFILE_WRITE_PERMISSION)?;
    validate_name(&request.name, "profile name")?;
    if let Some(display_name) = &request.display_name {
        validate_name(display_name, "profile display name")?;
    }
    let input = serde_json::to_value(request)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let inputs_hash = hash_json(&input)?;
    if let Some(result) = replay_mutation::<ProfileSummary>(
        &transaction,
        authorization,
        "profiles.create",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    ensure_game_exists(&transaction, &request.game_id)?;
    ensure_profile_name_available(
        &transaction,
        authorization.owner_scope(),
        &request.game_id,
        request.name.trim(),
        None,
    )?;
    let active = request.active.unwrap_or(false);
    if active {
        transaction.execute(
            "UPDATE library_profiles
             SET active = 0, revision = revision + 1, updated_at_ms = ?3
             WHERE owner_scope = ?1 AND game_id = ?2 AND active = 1
               AND deleted_at_ms IS NULL",
            params![authorization.owner_scope(), request.game_id, now],
        )?;
    }
    let profile_id = format!("profile:{}", Uuid::new_v4());
    let incarnation_id = format!("incarnation:profile:{}", Uuid::new_v4());
    let display_name = request
        .display_name
        .as_deref()
        .unwrap_or(request.name.as_str())
        .trim();
    let description = request.description.as_deref().unwrap_or("");
    transaction.execute(
        "INSERT INTO library_profiles(
           profile_id, game_id, owner_scope, name, display_name, active, description,
           revision, content_revision, lease_epoch, incarnation_id, last_used_at_ms,
           created_at_ms, updated_at_ms, deleted_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 1, 0, ?8, ?9, ?9, ?9, NULL)",
        params![
            profile_id,
            request.game_id,
            authorization.owner_scope(),
            request.name.trim(),
            display_name,
            active,
            description,
            incarnation_id,
            now
        ],
    )?;
    transaction.execute(
        "INSERT INTO library_groups(
           group_id, profile_id, parent_group_id, name, group_type, sort_order,
           expanded, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, NULL, 'Default', 'custom', 0, 1, ?3, ?3)",
        params![format!("group:{}", Uuid::new_v4()), profile_id, now],
    )?;
    let profile = load_profile(&transaction, &profile_id)?;
    let result = profile.summary();
    let operation_id = Uuid::new_v4().to_string();
    record_mutation(
        &transaction,
        authorization,
        "profiles.create",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.created",
        &json!({"operationId": operation_id, "profile": result}),
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn update_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesUpdateParams,
) -> Result<ProfileSummary, LibraryError> {
    authorization.require(PROFILE_WRITE_PERMISSION)?;
    if request.name.is_none() && request.display_name.is_none() && request.description.is_none()
    {
        return Err(LibraryError::InvalidRecord(
            "profile update has no fields".to_owned(),
        ));
    }
    if let Some(name) = &request.name {
        validate_name(name, "profile name")?;
    }
    if let Some(display_name) = &request.display_name {
        validate_name(display_name, "profile display name")?;
    }
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<ProfileSummary>(
        &transaction,
        authorization,
        "profiles.update",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    ensure_revision(request.expected_revision, profile.revision)?;
    if let Some(name) = &request.name {
        ensure_profile_name_available(
            &transaction,
            authorization.owner_scope(),
            &profile.game_id,
            name.trim(),
            Some(&profile.profile_id),
        )?;
    }
    let name = request.name.as_deref().unwrap_or(&profile.name).trim();
    let display_name = request
        .display_name
        .as_deref()
        .unwrap_or(&profile.display_name)
        .trim();
    let description = request
        .description
        .as_deref()
        .unwrap_or(&profile.description);
    let changed = name != profile.name
        || display_name != profile.display_name
        || description != profile.description;
    if changed {
        transaction.execute(
            "UPDATE library_profiles
             SET name = ?2, display_name = ?3, description = ?4,
                 revision = revision + 1, updated_at_ms = ?5
             WHERE profile_id = ?1",
            params![profile.profile_id, name, display_name, description, now],
        )?;
    }
    let result = load_profile(&transaction, &request.profile_id)?.summary();
    let operation_id = Uuid::new_v4().to_string();
    record_mutation(
        &transaction,
        authorization,
        "profiles.update",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.updated",
        &json!({"operationId": operation_id, "profile": result, "changed": changed}),
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn activate_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesActivateParams,
) -> Result<ProfileSummary, LibraryError> {
    authorization.require(PROFILE_WRITE_PERMISSION)?;
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<ProfileSummary>(
        &transaction,
        authorization,
        "profiles.activate",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    if profile.game_id != request.game_id {
        return Err(LibraryError::StaleTarget(request.profile_id.clone()));
    }
    ensure_revision(request.expected_revision, profile.revision)?;
    if !profile.active {
        transaction.execute(
            "UPDATE library_profiles
             SET active = 0, revision = revision + 1, updated_at_ms = ?3
             WHERE owner_scope = ?1 AND game_id = ?2 AND active = 1
               AND deleted_at_ms IS NULL",
            params![authorization.owner_scope(), request.game_id, now],
        )?;
        transaction.execute(
            "UPDATE library_profiles
             SET active = 1, revision = revision + 1, last_used_at_ms = ?2,
                 updated_at_ms = ?2
             WHERE profile_id = ?1",
            params![request.profile_id, now],
        )?;
    }
    let result = load_profile(&transaction, &request.profile_id)?.summary();
    let operation_id = Uuid::new_v4().to_string();
    record_mutation(
        &transaction,
        authorization,
        "profiles.activate",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.activated",
        &json!({"operationId": operation_id, "profile": result}),
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn delete_profile_command(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesDeleteParams,
) -> Result<LibraryMutationResult, LibraryError> {
    authorization.require(PROFILE_WRITE_PERMISSION)?;
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<LibraryMutationResult>(
        &transaction,
        authorization,
        "profiles.delete",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    ensure_revision(request.expected_revision, profile.revision)?;
    ensure_content_available(&transaction, &profile.profile_id)?;
    transaction.execute(
        "DELETE FROM library_profile_mods WHERE profile_id = ?1",
        params![profile.profile_id],
    )?;
    transaction.execute(
        "DELETE FROM library_groups WHERE profile_id = ?1",
        params![profile.profile_id],
    )?;
    let revision = profile.revision + 1;
    let content_revision = profile.content_revision + 1;
    transaction.execute(
        "UPDATE library_profiles
         SET active = 0, revision = ?2, content_revision = ?3,
             deleted_at_ms = ?4, updated_at_ms = ?4
         WHERE profile_id = ?1",
        params![profile.profile_id, revision, content_revision, now],
    )?;
    let operation_id = Uuid::new_v4().to_string();
    let result = LibraryMutationResult {
        operation_id: operation_id.clone(),
        profile_id: profile.profile_id.clone(),
        revision,
        content_revision,
        changed: true,
    };
    record_mutation(
        &transaction,
        authorization,
        "profiles.delete",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.deleted",
        &serde_json::to_value(&result)?,
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn clone_profile(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ProfilesCloneParams,
) -> Result<ProfileSummary, LibraryError> {
    authorization.require(PROFILE_WRITE_PERMISSION)?;
    validate_name(&request.name, "profile name")?;
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<ProfileSummary>(
        &transaction,
        authorization,
        "profiles.clone",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let source = load_profile(&transaction, &request.source_profile_id)?;
    authorize_profile(authorization, &source)?;
    ensure_revision(request.expected_content_revision, source.content_revision)?;
    ensure_profile_name_available(
        &transaction,
        authorization.owner_scope(),
        &source.game_id,
        request.name.trim(),
        None,
    )?;
    let active = request.active.unwrap_or(false);
    if active {
        transaction.execute(
            "UPDATE library_profiles
             SET active = 0, revision = revision + 1, updated_at_ms = ?3
             WHERE owner_scope = ?1 AND game_id = ?2 AND active = 1
               AND deleted_at_ms IS NULL",
            params![authorization.owner_scope(), source.game_id, now],
        )?;
    }
    let target_id = format!("profile:{}", Uuid::new_v4());
    let target_incarnation = format!("incarnation:profile:{}", Uuid::new_v4());
    transaction.execute(
        "INSERT INTO library_profiles(
           profile_id, game_id, owner_scope, name, display_name, active, description,
           revision, content_revision, lease_epoch, incarnation_id, last_used_at_ms,
           created_at_ms, updated_at_ms, deleted_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 1, 0, ?8, ?9, ?9, ?9, NULL)",
        params![
            target_id,
            source.game_id,
            authorization.owner_scope(),
            request.name.trim(),
            request
                .display_name
                .as_deref()
                .unwrap_or(request.name.as_str())
                .trim(),
            active,
            request
                .description
                .as_deref()
                .unwrap_or(&source.description),
            target_incarnation,
            now
        ],
    )?;
    let groups = load_group_rows(&transaction, &source.profile_id)?;
    let group_ids = groups
        .iter()
        .map(|group| (group.group_id.clone(), format!("group:{}", Uuid::new_v4())))
        .collect::<HashMap<_, _>>();
    if groups.is_empty() {
        transaction.execute(
            "INSERT INTO library_groups VALUES (?1, ?2, NULL, 'Default', 'custom', 0, 1, ?3, ?3)",
            params![format!("group:{}", Uuid::new_v4()), target_id, now],
        )?;
    } else {
        for group in groups {
            let target_group_id = group_ids.get(&group.group_id).ok_or_else(|| {
                LibraryError::InvalidRecord("cloned group mapping is incomplete".to_owned())
            })?;
            let target_parent_group_id = group
                .parent_group_id
                .as_ref()
                .map(|id| {
                    group_ids.get(id).ok_or_else(|| {
                        LibraryError::InvalidRecord(
                            "cloned parent group mapping is incomplete".to_owned(),
                        )
                    })
                })
                .transpose()?;
            transaction.execute(
                "INSERT INTO library_groups(
                   group_id, profile_id, parent_group_id, name, group_type, sort_order,
                   expanded, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    target_group_id,
                    target_id,
                    target_parent_group_id,
                    group.name,
                    group.group_type,
                    group.sort_order,
                    group.expanded,
                    now
                ],
            )?;
        }
    }
    let memberships = load_profile_mod_rows(&transaction, &source.profile_id)?;
    for membership in memberships {
        let target_group_id = membership
            .group_id
            .as_ref()
            .map(|id| {
                group_ids.get(id).ok_or_else(|| {
                    LibraryError::InvalidRecord(
                        "cloned membership group mapping is incomplete".to_owned(),
                    )
                })
            })
            .transpose()?;
        transaction.execute(
            "INSERT INTO library_profile_mods(
               profile_mod_id, profile_id, mod_id, group_id, incarnation_id, sort_order,
               enabled, used_version, version_selection_kind, selected_version_id,
               created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?10, ?10)",
            params![
                format!("profile-mod:{}", Uuid::new_v4()),
                target_id,
                membership.mod_id,
                target_group_id,
                format!("incarnation:profile-mod:{}", Uuid::new_v4()),
                membership.sort_order,
                membership.enabled,
                membership.version_selection.kind_name(),
                membership.version_selection.version_id,
                now
            ],
        )?;
    }
    let result = load_profile(&transaction, &target_id)?.summary();
    let operation_id = Uuid::new_v4().to_string();
    record_mutation(
        &transaction,
        authorization,
        "profiles.clone",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.cloned",
        &json!({
            "operationId": operation_id,
            "sourceProfileId": source.profile_id,
            "profile": result,
        }),
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn list_groups(
    &self,
    authorization: &LibraryAuthorization,
    request: &GroupsListParams,
) -> Result<GroupsListResult, LibraryError> {
    authorization.require(READ_PERMISSION)?;
    let connection = self.connection()?;
    let profile = load_profile_connection(&connection, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    let mut statement = connection.prepare(
        "SELECT group_id, profile_id, parent_group_id, name, group_type, sort_order,
                expanded
         FROM library_groups WHERE profile_id = ?1
         ORDER BY sort_order, group_id",
    )?;
    let rows = statement.query_map(params![request.profile_id], map_group_summary)?;
    Ok(GroupsListResult {
        profile_id: profile.profile_id,
        content_revision: profile.content_revision,
        items: rows.collect::<Result<_, _>>()?,
    })
}

pub fn create_group(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsCreateParams,
) -> Result<GroupSummary, LibraryError> {
    authorization.require(CONTENT_WRITE_PERMISSION)?;
    validate_name(&request.name, "group name")?;
    validate_name(&request.group_type, "group type")?;
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<GroupSummary>(
        &transaction,
        authorization,
        "groups.create",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    ensure_content_available(&transaction, &profile.profile_id)?;
    validate_parent_group(
        &transaction,
        &profile.profile_id,
        request.parent_group_id.as_deref(),
    )?;
    let group_id = format!("group:{}", Uuid::new_v4());
    transaction.execute(
        "INSERT INTO library_groups(
           group_id, profile_id, parent_group_id, name, group_type, sort_order,
           expanded, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?7)",
        params![
            group_id,
            profile.profile_id,
            request.parent_group_id,
            request.name.trim(),
            request.group_type.trim(),
            request.expanded.unwrap_or(true),
            now
        ],
    )?;
    reorder_groups(
        &transaction,
        &profile.profile_id,
        request.parent_group_id.as_deref(),
        &group_id,
        request.position,
        now,
    )?;
    bump_content_revision(&transaction, &profile.profile_id, now)?;
    let result = load_group(&transaction, &group_id)?;
    let operation_id = Uuid::new_v4().to_string();
    record_mutation(
        &transaction,
        authorization,
        "groups.create",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.content.changed",
        &json!({"operationId": operation_id, "profileId": profile.profile_id}),
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn update_group(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsUpdateParams,
) -> Result<GroupSummary, LibraryError> {
    authorization.require(CONTENT_WRITE_PERMISSION)?;
    if request.name.is_none() && request.expanded.is_none() {
        return Err(LibraryError::InvalidRecord(
            "group update has no fields".to_owned(),
        ));
    }
    if let Some(name) = &request.name {
        validate_name(name, "group name")?;
    }
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<GroupSummary>(
        &transaction,
        authorization,
        "groups.update",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    ensure_content_available(&transaction, &profile.profile_id)?;
    let group = load_group(&transaction, &request.group_id)?;
    if group.profile_id != profile.profile_id {
        return Err(LibraryError::StaleTarget(request.group_id.clone()));
    }
    let name = request.name.as_deref().unwrap_or(&group.name).trim();
    let expanded = request.expanded.unwrap_or(group.expanded);
    let changed = name != group.name || expanded != group.expanded;
    if changed {
        transaction.execute(
            "UPDATE library_groups SET name = ?2, expanded = ?3, updated_at_ms = ?4
             WHERE group_id = ?1",
            params![group.group_id, name, expanded, now],
        )?;
        bump_content_revision(&transaction, &profile.profile_id, now)?;
    }
    let result = load_group(&transaction, &request.group_id)?;
    let operation_id = Uuid::new_v4().to_string();
    record_mutation(
        &transaction,
        authorization,
        "groups.update",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.content.changed",
        &json!({
            "operationId": operation_id,
            "profileId": profile.profile_id,
            "changed": changed,
        }),
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn move_group(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsMoveParams,
) -> Result<LibraryMutationResult, LibraryError> {
    authorization.require(CONTENT_WRITE_PERMISSION)?;
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<LibraryMutationResult>(
        &transaction,
        authorization,
        "groups.move",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    ensure_content_available(&transaction, &profile.profile_id)?;
    let group = load_group(&transaction, &request.group_id)?;
    if group.profile_id != profile.profile_id {
        return Err(LibraryError::StaleTarget(request.group_id.clone()));
    }
    validate_parent_group(
        &transaction,
        &profile.profile_id,
        request.parent_group_id.as_deref(),
    )?;
    ensure_no_group_cycle(
        &transaction,
        &group.group_id,
        request.parent_group_id.as_deref(),
    )?;
    transaction.execute(
        "UPDATE library_groups SET parent_group_id = ?2, updated_at_ms = ?3
         WHERE group_id = ?1",
        params![group.group_id, request.parent_group_id, now],
    )?;
    reorder_groups(
        &transaction,
        &profile.profile_id,
        group.parent_group_id.as_deref(),
        &group.group_id,
        u64::MAX,
        now,
    )?;
    reorder_groups(
        &transaction,
        &profile.profile_id,
        request.parent_group_id.as_deref(),
        &group.group_id,
        request.position,
        now,
    )?;
    let content_revision = bump_content_revision(&transaction, &profile.profile_id, now)?;
    let operation_id = Uuid::new_v4().to_string();
    let result = LibraryMutationResult {
        operation_id: operation_id.clone(),
        profile_id: profile.profile_id.clone(),
        revision: profile.revision,
        content_revision,
        changed: true,
    };
    record_mutation(
        &transaction,
        authorization,
        "groups.move",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.content.changed",
        &serde_json::to_value(&result)?,
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn delete_group_command(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &GroupsDeleteParams,
) -> Result<LibraryMutationResult, LibraryError> {
    authorization.require(CONTENT_WRITE_PERMISSION)?;
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<LibraryMutationResult>(
        &transaction,
        authorization,
        "groups.delete",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    ensure_content_available(&transaction, &profile.profile_id)?;
    let group = load_group(&transaction, &request.group_id)?;
    if group.profile_id != profile.profile_id {
        return Err(LibraryError::StaleTarget(request.group_id.clone()));
    }
    let group_count: u64 = transaction.query_row(
        "SELECT COUNT(*) FROM library_groups WHERE profile_id = ?1",
        params![profile.profile_id],
        |row| row.get(0),
    )?;
    if group_count <= 1 {
        return Err(LibraryError::InvalidRecord(
            "a profile must retain at least one group".to_owned(),
        ));
    }
    let has_children: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_groups WHERE parent_group_id = ?1)",
        params![group.group_id],
        |row| row.get(0),
    )?;
    if has_children {
        return Err(LibraryError::InvalidRecord(
            "group with child groups cannot be deleted".to_owned(),
        ));
    }
    transaction.execute(
        "DELETE FROM library_groups WHERE group_id = ?1",
        params![group.group_id],
    )?;
    normalize_groups(
        &transaction,
        &profile.profile_id,
        group.parent_group_id.as_deref(),
        now,
    )?;
    let content_revision = bump_content_revision(&transaction, &profile.profile_id, now)?;
    let operation_id = Uuid::new_v4().to_string();
    let result = LibraryMutationResult {
        operation_id: operation_id.clone(),
        profile_id: profile.profile_id.clone(),
        revision: profile.revision,
        content_revision,
        changed: true,
    };
    record_mutation(
        &transaction,
        authorization,
        "groups.delete",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.content.changed",
        &serde_json::to_value(&result)?,
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn list_mods(
    &self,
    authorization: &LibraryAuthorization,
    request: &ModsListParams,
) -> Result<ModsListResult, LibraryError> {
    authorization.require(READ_PERMISSION)?;
    validate_page_size(request.page_size)?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    let scope_hash = hash_json(&json!({
        "profileId": request.profile_id,
        "query": request.query,
        "sourceId": request.source_id,
        "enabled": request.enabled,
        "groupId": request.group_id,
    }))?;
    let snapshot = profile.content_revision.to_string();
    let offset = cursor_offset(
        request.cursor.as_deref(),
        "mods.list",
        &scope_hash,
        &snapshot,
    )?;
    let query = request
        .query
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let total: u64 = transaction.query_row(
        "SELECT COUNT(*)
         FROM library_profile_mods pm
         JOIN library_mods m ON m.mod_id = pm.mod_id
         WHERE pm.profile_id = ?1
           AND (?2 IS NULL OR instr(lower(m.display_name), lower(?2)) > 0)
           AND (?3 IS NULL OR m.source_id = ?3)
           AND (?4 IS NULL OR pm.enabled = ?4)
           AND (?5 IS NULL OR pm.group_id = ?5)",
        params![
            request.profile_id,
            query,
            request.source_id,
            request.enabled,
            request.group_id
        ],
        |row| row.get(0),
    )?;
    let mut statement = transaction.prepare(
        "SELECT pm.profile_mod_id, pm.incarnation_id, m.mod_id, m.source_id,
                m.remote_game_key, m.remote_mod_id, m.display_name, m.revision,
                pm.enabled, pm.sort_order, pm.group_id, pm.version_selection_kind,
                pm.selected_version_id, selected.remote_file_id, binding.version_id,
                m.tags_json, m.approval_status,
                (SELECT CASE WHEN count(*) = 1 THEN NULLIF(NULLIF(max(version_id), '-'), 'legacy-unversioned') END
                 FROM library_mod_versions WHERE mod_id = m.mod_id AND current = 1)
         FROM library_profile_mods pm
         JOIN library_mods m ON m.mod_id = pm.mod_id
         LEFT JOIN library_mod_versions selected
           ON selected.mod_id = pm.mod_id AND selected.version_id = pm.selected_version_id
         LEFT JOIN library_profile_content_bindings binding
           ON binding.profile_mod_id = pm.profile_mod_id
         WHERE pm.profile_id = ?1
           AND (?2 IS NULL OR instr(lower(m.display_name), lower(?2)) > 0)
           AND (?3 IS NULL OR m.source_id = ?3)
           AND (?4 IS NULL OR pm.enabled = ?4)
           AND (?5 IS NULL OR pm.group_id = ?5)
         ORDER BY pm.sort_order, pm.profile_mod_id
         LIMIT ?6 OFFSET ?7",
    )?;
    let rows = statement.query_map(
        params![
            request.profile_id,
            query,
            request.source_id,
            request.enabled,
            request.group_id,
            request.page_size + 1,
            offset
        ],
        map_mod_summary,
    )?;
    let mut items = rows.collect::<Result<Vec<_>, _>>()?;
    let has_more = items.len() as u64 > request.page_size;
    items.truncate(request.page_size as usize);
    let next_cursor = has_more
        .then(|| {
            encode_cursor(
                "mods.list",
                &scope_hash,
                &snapshot,
                offset + request.page_size,
            )
        })
        .transpose()?;
    drop(statement);
    transaction.commit()?;
    Ok(ModsListResult {
        profile_id: profile.profile_id,
        content_revision: profile.content_revision,
        total,
        items,
        next_cursor,
    })
}

pub fn get_mod(
    &self,
    authorization: &LibraryAuthorization,
    request: &ModsGetParams,
) -> Result<ModDetails, LibraryError> {
    authorization.require(READ_PERMISSION)?;
    let connection = self.connection()?;
    let profile = load_profile_connection(&connection, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    let summary = query_mod_summary(&connection, &request.profile_id, &request.profile_mod_id)?;
    let mut statement = connection.prepare(
        "SELECT version_id, remote_file_id, artifact_digest, availability, current, file_size
         FROM library_mod_versions WHERE mod_id = ?1 ORDER BY version_id",
    )?;
    let rows = statement.query_map(params![summary.mod_id], |row| {
        Ok(ModVersionSummary {
            version_id: row.get(0)?,
            remote_file_id: row.get(1)?,
            artifact_digest: row.get(2)?,
            availability: row.get(3)?,
            current: row.get(4)?,
            file_size: row.get(5)?,
        })
    })?;
    Ok(ModDetails {
        summary,
        versions: rows.collect::<Result<_, _>>()?,
    })
}

pub fn rename_mod(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsRenameParams,
) -> Result<ModEntityResult, LibraryError> {
    authorization.require(METADATA_WRITE_PERMISSION)?;
    validate_name(&request.display_name, "mod display name")?;
    if request.scope != "shared" {
        return Err(LibraryError::InvalidRecord(
            "mod rename scope must be shared".to_owned(),
        ));
    }
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<ModEntityResult>(
        &transaction,
        authorization,
        "mods.rename",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let current = transaction
        .query_row(
            "SELECT display_name, revision FROM library_mods WHERE mod_id = ?1",
            params![request.mod_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(request.mod_id.clone()))?;
    let owned: bool = transaction.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM library_profile_mods pm
           JOIN library_profiles p ON p.profile_id = pm.profile_id
           WHERE pm.mod_id = ?1 AND p.owner_scope = ?2 AND p.deleted_at_ms IS NULL
         )",
        params![request.mod_id, authorization.owner_scope()],
        |row| row.get(0),
    )?;
    if !owned {
        return Err(LibraryError::PermissionDenied(request.mod_id.clone()));
    }
    ensure_revision(request.expected_entity_revision, current.1)?;
    let changed = current.0 != request.display_name.trim();
    let entity_revision = if changed {
        let next = current.1 + 1;
        transaction.execute(
            "UPDATE library_mods SET display_name = ?2, revision = ?3, updated_at_ms = ?4
             WHERE mod_id = ?1",
            params![request.mod_id, request.display_name.trim(), next, now],
        )?;
        next
    } else {
        current.1
    };
    let operation_id = Uuid::new_v4().to_string();
    let result = ModEntityResult {
        operation_id: operation_id.clone(),
        mod_id: request.mod_id.clone(),
        entity_revision,
        changed,
    };
    record_mutation(
        &transaction,
        authorization,
        "mods.rename",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "mod.renamed",
        &serde_json::to_value(&result)?,
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn set_mod_enabled(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsSetEnabledParams,
) -> Result<LibraryMutationResult, LibraryError> {
    self.set_mods_enabled(
        authorization,
        idempotency_key,
        SetModsEnabled {
            scope: "mods.set-enabled",
            profile_id: &request.profile_id,
            profile_mod_ids: std::slice::from_ref(&request.profile_mod_id),
            expected_content_revision: request.expected_content_revision,
            enabled: request.enabled,
        },
    )
}

pub fn set_mods_enabled_batch(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsSetEnabledBatchParams,
) -> Result<LibraryMutationResult, LibraryError> {
    self.set_mods_enabled(
        authorization,
        idempotency_key,
        SetModsEnabled {
            scope: "mods.set-enabled-batch",
            profile_id: &request.profile_id,
            profile_mod_ids: &request.profile_mod_ids,
            expected_content_revision: request.expected_content_revision,
            enabled: request.enabled,
        },
    )
}

fn set_mods_enabled(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    change: SetModsEnabled<'_>,
) -> Result<LibraryMutationResult, LibraryError> {
    authorization.require(CONTENT_WRITE_PERMISSION)?;
    let unique = change.profile_mod_ids.iter().collect::<HashSet<_>>();
    if unique.is_empty() || unique.len() != change.profile_mod_ids.len() {
        return Err(LibraryError::InvalidRecord(
            "profile mod ids must be non-empty and unique".to_owned(),
        ));
    }
    let input = json!({
        "profileId": change.profile_id,
        "profileModIds": change.profile_mod_ids,
        "expectedContentRevision": change.expected_content_revision,
        "enabled": change.enabled,
    });
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<LibraryMutationResult>(
        &transaction,
        authorization,
        change.scope,
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, change.profile_id)?;
    authorize_profile(authorization, &profile)?;
    ensure_revision(change.expected_content_revision, profile.content_revision)?;
    ensure_content_available(&transaction, change.profile_id)?;
    let mut changed = false;
    for profile_mod_id in change.profile_mod_ids {
        let current = transaction
            .query_row(
                "SELECT enabled FROM library_profile_mods
                 WHERE profile_mod_id = ?1 AND profile_id = ?2",
                params![profile_mod_id, change.profile_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .ok_or_else(|| LibraryError::NotFound(profile_mod_id.clone()))?;
        if current != change.enabled {
            transaction.execute(
                "UPDATE library_profile_mods SET enabled = ?2, updated_at_ms = ?3
                 WHERE profile_mod_id = ?1",
                params![profile_mod_id, change.enabled, now],
            )?;
            changed = true;
        }
    }
    let content_revision = if changed {
        bump_content_revision(&transaction, change.profile_id, now)?
    } else {
        profile.content_revision
    };
    let operation_id = Uuid::new_v4().to_string();
    let result = LibraryMutationResult {
        operation_id: operation_id.clone(),
        profile_id: change.profile_id.to_owned(),
        revision: profile.revision,
        content_revision,
        changed,
    };
    record_mutation(
        &transaction,
        authorization,
        change.scope,
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.content.changed",
        &serde_json::to_value(&result)?,
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn move_mod(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsMoveParams,
) -> Result<LibraryMutationResult, LibraryError> {
    authorization.require(CONTENT_WRITE_PERMISSION)?;
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<LibraryMutationResult>(
        &transaction,
        authorization,
        "mods.move",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    ensure_content_available(&transaction, &profile.profile_id)?;
    validate_parent_group(
        &transaction,
        &profile.profile_id,
        request.group_id.as_deref(),
    )?;
    let old_group = transaction
        .query_row(
            "SELECT group_id FROM library_profile_mods
             WHERE profile_mod_id = ?1 AND profile_id = ?2",
            params![request.profile_mod_id, request.profile_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(request.profile_mod_id.clone()))?;
    transaction.execute(
        "UPDATE library_profile_mods SET group_id = ?2, updated_at_ms = ?3
         WHERE profile_mod_id = ?1",
        params![request.profile_mod_id, request.group_id, now],
    )?;
    reorder_profile_mods(
        &transaction,
        &profile.profile_id,
        old_group.as_deref(),
        &request.profile_mod_id,
        u64::MAX,
        now,
    )?;
    reorder_profile_mods(
        &transaction,
        &profile.profile_id,
        request.group_id.as_deref(),
        &request.profile_mod_id,
        request.position,
        now,
    )?;
    let content_revision = bump_content_revision(&transaction, &profile.profile_id, now)?;
    let operation_id = Uuid::new_v4().to_string();
    let result = LibraryMutationResult {
        operation_id: operation_id.clone(),
        profile_id: profile.profile_id.clone(),
        revision: profile.revision,
        content_revision,
        changed: true,
    };
    record_mutation(
        &transaction,
        authorization,
        "mods.move",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.content.changed",
        &serde_json::to_value(&result)?,
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn remove_mod(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsRemoveParams,
) -> Result<LibraryMutationResult, LibraryError> {
    authorization.require(CONTENT_WRITE_PERMISSION)?;
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<LibraryMutationResult>(
        &transaction,
        authorization,
        "mods.remove",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    ensure_content_available(&transaction, &profile.profile_id)?;
    let old_group = transaction
        .query_row(
            "SELECT group_id FROM library_profile_mods
             WHERE profile_mod_id = ?1 AND profile_id = ?2",
            params![request.profile_mod_id, request.profile_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(request.profile_mod_id.clone()))?;
    transaction.execute(
        "DELETE FROM library_profile_mods WHERE profile_mod_id = ?1",
        params![request.profile_mod_id],
    )?;
    normalize_profile_mods(&transaction, &profile.profile_id, old_group.as_deref(), now)?;
    let content_revision = bump_content_revision(&transaction, &profile.profile_id, now)?;
    let operation_id = Uuid::new_v4().to_string();
    let result = LibraryMutationResult {
        operation_id: operation_id.clone(),
        profile_id: profile.profile_id.clone(),
        revision: profile.revision,
        content_revision,
        changed: true,
    };
    record_mutation(
        &transaction,
        authorization,
        "mods.remove",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.content.changed",
        &serde_json::to_value(&result)?,
        now,
    )?;
    transaction.commit()?;
    Ok(result)
}

pub fn cleanup_missing_mods(
    &self,
    authorization: &LibraryAuthorization,
    idempotency_key: &str,
    request: &ModsCleanupMissingParams,
) -> Result<LibraryMutationResult, LibraryError> {
    authorization.require(CONTENT_WRITE_PERMISSION)?;
    let input = serde_json::to_value(request)?;
    let inputs_hash = hash_json(&input)?;
    let now = now_ms()?;
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(result) = replay_mutation::<LibraryMutationResult>(
        &transaction,
        authorization,
        "mods.cleanup-missing",
        idempotency_key,
        &inputs_hash,
    )? {
        transaction.commit()?;
        return Ok(result);
    }
    let profile = load_profile(&transaction, &request.profile_id)?;
    authorize_profile(authorization, &profile)?;
    if profile.game_id != request.game_id {
        return Err(LibraryError::InvalidRecord(
            "profile does not belong to the explicit game".to_owned(),
        ));
    }
    ensure_revision(request.expected_content_revision, profile.content_revision)?;
    ensure_content_available(&transaction, &profile.profile_id)?;
    let groups = {
        let mut statement = transaction.prepare(
            "SELECT DISTINCT pm.group_id
             FROM library_profile_mods pm
             JOIN library_mods m ON m.mod_id = pm.mod_id
             WHERE pm.profile_id = ?1 AND m.game_id = ?2 AND m.local_missing = 1",
        )?;
        let rows = statement
            .query_map(params![request.profile_id, request.game_id], |row| {
                row.get::<_, Option<String>>(0)
            })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let removed = transaction.execute(
        "DELETE FROM library_profile_mods
         WHERE profile_id = ?1 AND mod_id IN (
             SELECT mod_id FROM library_mods
             WHERE game_id = ?2 AND local_missing = 1
         )",
        params![request.profile_id, request.game_id],
    )?;
    for group_id in groups {
        normalize_profile_mods(&transaction, &profile.profile_id, group_id.as_deref(), now)?;
    }
    let changed = removed > 0;
    let content_revision = if changed {
        bump_content_revision(&transaction, &profile.profile_id, now)?
    } else {
        profile.content_revision
    };
    let operation_id = Uuid::new_v4().to_string();
    let result = LibraryMutationResult {
        operation_id: operation_id.clone(),
        profile_id: profile.profile_id.clone(),
        revision: profile.revision,
        content_revision,
        changed,
    };
    record_mutation(
        &transaction,
        authorization,
        "mods.cleanup-missing",
        idempotency_key,
        &inputs_hash,
        &operation_id,
        &result,
        now,
    )?;
    insert_application_event(
        &transaction,
        "profile.content.changed",
        &serde_json::to_value(&result)?,
        now,
    )?;
    transaction.commit()?;
    Ok(result)
} }

#[derive(Debug)]
struct GroupRow {
    group_id: String,
    profile_id: String,
    parent_group_id: Option<String>,
    name: String,
    group_type: String,
    sort_order: u64,
    expanded: bool,
}

impl From<GroupRow> for GroupSummary {
    fn from(value: GroupRow) -> Self {
        Self {
            group_id: value.group_id,
            profile_id: value.profile_id,
            parent_group_id: value.parent_group_id,
            name: value.name,
            group_type: value.group_type,
            sort_order: value.sort_order,
            expanded: value.expanded,
        }
    }
}

#[derive(Debug)]
struct ProfileModCloneRow {
    mod_id: String,
    group_id: Option<String>,
    sort_order: u64,
    enabled: bool,
    version_selection: VersionSelection,
}

trait VersionSelectionExt {
    fn kind_name(&self) -> &'static str;
}

impl VersionSelectionExt for VersionSelection {
    fn kind_name(&self) -> &'static str {
        match self.kind {
            VersionSelectionKind::Auto => "auto",
            VersionSelectionKind::Pinned => "pinned",
        }
    }
}

pub(crate) fn load_profile(
    transaction: &Transaction<'_>,
    profile_id: &str,
) -> Result<ProfileRow, LibraryError> {
    load_profile_connection(transaction, profile_id)
}

fn load_profile_connection(
    connection: &rusqlite::Connection,
    profile_id: &str,
) -> Result<ProfileRow, LibraryError> {
    connection
        .query_row(
            "SELECT profile_id, game_id, owner_scope, name, display_name, active,
                    description, revision, content_revision, incarnation_id
             FROM library_profiles
             WHERE profile_id = ?1 AND deleted_at_ms IS NULL",
            params![profile_id],
            map_profile_row,
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(profile_id.to_owned()))
}

fn map_profile_row(row: &rusqlite::Row<'_>) -> Result<ProfileRow, rusqlite::Error> {
    Ok(ProfileRow {
        profile_id: row.get(0)?,
        game_id: row.get(1)?,
        owner_scope: row.get(2)?,
        name: row.get(3)?,
        display_name: row.get(4)?,
        active: row.get(5)?,
        description: row.get(6)?,
        revision: row.get(7)?,
        content_revision: row.get(8)?,
        incarnation_id: row.get(9)?,
    })
}

pub(crate) fn authorize_profile(
    authorization: &LibraryAuthorization,
    profile: &ProfileRow,
) -> Result<(), LibraryError> {
    (authorization.owner_scope() == profile.owner_scope)
        .then_some(())
        .ok_or_else(|| LibraryError::PermissionDenied(profile.profile_id.clone()))
}

pub(crate) fn ensure_revision(expected: u64, actual: u64) -> Result<(), LibraryError> {
    (expected == actual)
        .then_some(())
        .ok_or(LibraryError::RevisionConflict { expected, actual })
}

fn ensure_game_exists(transaction: &Transaction<'_>, game_id: &str) -> Result<(), LibraryError> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_games WHERE game_id = ?1)",
        params![game_id],
        |row| row.get(0),
    )?;
    exists
        .then_some(())
        .ok_or_else(|| LibraryError::NotFound(game_id.to_owned()))
}

fn ensure_profile_name_available(
    transaction: &Transaction<'_>,
    owner_scope: &str,
    game_id: &str,
    name: &str,
    excluded_profile_id: Option<&str>,
) -> Result<(), LibraryError> {
    let existing = transaction
        .query_row(
            "SELECT profile_id FROM library_profiles
             WHERE owner_scope = ?1 AND game_id = ?2 AND name = ?3 COLLATE NOCASE
               AND deleted_at_ms IS NULL AND (?4 IS NULL OR profile_id <> ?4)
             LIMIT 1",
            params![owner_scope, game_id, name, excluded_profile_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if existing.is_some() {
        return Err(LibraryError::InvalidRecord(format!(
            "profile name already exists: {name}"
        )));
    }
    Ok(())
}

pub(crate) fn ensure_content_available(
    transaction: &Transaction<'_>,
    profile_id: &str,
) -> Result<(), LibraryError> {
    let operation = transaction
        .query_row(
            "SELECT owner_id FROM (
               SELECT operation_id AS owner_id
               FROM library_operation_leases
               WHERE resource_key = ?1 AND state IN ('active', 'suspect')
               UNION ALL
               SELECT guard_id AS owner_id
               FROM library_profile_content_guards
               WHERE resource_key = ?1 AND state = 'active'
             ) LIMIT 1",
            params![format!("profile-content:{profile_id}")],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(owner_operation_id) = operation {
        return Err(LibraryError::ResourceBusy { owner_operation_id });
    }
    Ok(())
}

pub(crate) fn bump_content_revision(
    transaction: &Transaction<'_>,
    profile_id: &str,
    now: i64,
) -> Result<u64, LibraryError> {
    transaction.execute(
        "UPDATE library_profiles
         SET content_revision = content_revision + 1, updated_at_ms = ?2
         WHERE profile_id = ?1 AND deleted_at_ms IS NULL",
        params![profile_id, now],
    )?;
    Ok(transaction.query_row(
        "SELECT content_revision FROM library_profiles WHERE profile_id = ?1",
        params![profile_id],
        |row| row.get(0),
    )?)
}

fn validate_parent_group(
    transaction: &Transaction<'_>,
    profile_id: &str,
    group_id: Option<&str>,
) -> Result<(), LibraryError> {
    let Some(group_id) = group_id else {
        return Ok(());
    };
    let belongs: bool = transaction.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM library_groups WHERE group_id = ?1 AND profile_id = ?2
         )",
        params![group_id, profile_id],
        |row| row.get(0),
    )?;
    belongs
        .then_some(())
        .ok_or_else(|| LibraryError::NotFound(group_id.to_owned()))
}

fn ensure_no_group_cycle(
    transaction: &Transaction<'_>,
    moving_group_id: &str,
    parent_group_id: Option<&str>,
) -> Result<(), LibraryError> {
    let mut current_parent = parent_group_id.map(str::to_owned);
    while let Some(parent) = current_parent {
        if parent == moving_group_id {
            return Err(LibraryError::InvalidRecord(
                "group move would create a cycle".to_owned(),
            ));
        }
        current_parent = transaction
            .query_row(
                "SELECT parent_group_id FROM library_groups WHERE group_id = ?1",
                params![parent],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
    }
    Ok(())
}

fn load_group(transaction: &Transaction<'_>, group_id: &str) -> Result<GroupSummary, LibraryError> {
    transaction
        .query_row(
            "SELECT group_id, profile_id, parent_group_id, name, group_type, sort_order,
                    expanded
             FROM library_groups WHERE group_id = ?1",
            params![group_id],
            map_group_summary,
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(group_id.to_owned()))
}

fn map_group_summary(row: &rusqlite::Row<'_>) -> Result<GroupSummary, rusqlite::Error> {
    Ok(GroupSummary {
        group_id: row.get(0)?,
        profile_id: row.get(1)?,
        parent_group_id: row.get(2)?,
        name: row.get(3)?,
        group_type: row.get(4)?,
        sort_order: row.get(5)?,
        expanded: row.get(6)?,
    })
}

fn load_group_rows(
    transaction: &Transaction<'_>,
    profile_id: &str,
) -> Result<Vec<GroupRow>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT group_id, profile_id, parent_group_id, name, group_type, sort_order,
                expanded
         FROM library_groups WHERE profile_id = ?1 ORDER BY sort_order, group_id",
    )?;
    let rows = statement.query_map(params![profile_id], |row| {
        Ok(GroupRow {
            group_id: row.get(0)?,
            profile_id: row.get(1)?,
            parent_group_id: row.get(2)?,
            name: row.get(3)?,
            group_type: row.get(4)?,
            sort_order: row.get(5)?,
            expanded: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn load_profile_mod_rows(
    transaction: &Transaction<'_>,
    profile_id: &str,
) -> Result<Vec<ProfileModCloneRow>, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT mod_id, group_id, sort_order, enabled, version_selection_kind,
                selected_version_id
         FROM library_profile_mods WHERE profile_id = ?1 ORDER BY sort_order, profile_mod_id",
    )?;
    let rows = statement.query_map(params![profile_id], |row| {
        let kind: String = row.get(4)?;
        Ok(ProfileModCloneRow {
            mod_id: row.get(0)?,
            group_id: row.get(1)?,
            sort_order: row.get(2)?,
            enabled: row.get(3)?,
            version_selection: VersionSelection {
                kind: if kind == "pinned" {
                    VersionSelectionKind::Pinned
                } else {
                    VersionSelectionKind::Auto
                },
                version_id: row.get(5)?,
            },
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn reorder_groups(
    transaction: &Transaction<'_>,
    profile_id: &str,
    parent_group_id: Option<&str>,
    moving_group_id: &str,
    position: u64,
    now: i64,
) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT group_id FROM library_groups
         WHERE profile_id = ?1 AND parent_group_id IS ?2 AND group_id <> ?3
         ORDER BY sort_order, group_id",
    )?;
    let rows = statement.query_map(
        params![profile_id, parent_group_id, moving_group_id],
        |row| row.get::<_, String>(0),
    )?;
    let mut ids = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let index = usize::try_from(position)
        .unwrap_or(usize::MAX)
        .min(ids.len());
    ids.insert(index, moving_group_id.to_owned());
    update_group_order(transaction, &ids, now)
}

fn normalize_groups(
    transaction: &Transaction<'_>,
    profile_id: &str,
    parent_group_id: Option<&str>,
    now: i64,
) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT group_id FROM library_groups
         WHERE profile_id = ?1 AND parent_group_id IS ?2
         ORDER BY sort_order, group_id",
    )?;
    let rows = statement.query_map(params![profile_id, parent_group_id], |row| {
        row.get::<_, String>(0)
    })?;
    let ids = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    update_group_order(transaction, &ids, now)
}

fn update_group_order(
    transaction: &Transaction<'_>,
    ids: &[String],
    now: i64,
) -> Result<(), LibraryError> {
    for (index, id) in ids.iter().enumerate() {
        transaction.execute(
            "UPDATE library_groups SET sort_order = ?2, updated_at_ms = ?3
             WHERE group_id = ?1",
            params![id, index as u64, now],
        )?;
    }
    Ok(())
}

fn reorder_profile_mods(
    transaction: &Transaction<'_>,
    profile_id: &str,
    group_id: Option<&str>,
    moving_profile_mod_id: &str,
    position: u64,
    now: i64,
) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT profile_mod_id FROM library_profile_mods
         WHERE profile_id = ?1 AND group_id IS ?2 AND profile_mod_id <> ?3
         ORDER BY sort_order, profile_mod_id",
    )?;
    let rows = statement.query_map(
        params![profile_id, group_id, moving_profile_mod_id],
        |row| row.get::<_, String>(0),
    )?;
    let mut ids = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let index = usize::try_from(position)
        .unwrap_or(usize::MAX)
        .min(ids.len());
    ids.insert(index, moving_profile_mod_id.to_owned());
    update_profile_mod_order(transaction, &ids, now)
}

fn normalize_profile_mods(
    transaction: &Transaction<'_>,
    profile_id: &str,
    group_id: Option<&str>,
    now: i64,
) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT profile_mod_id FROM library_profile_mods
         WHERE profile_id = ?1 AND group_id IS ?2
         ORDER BY sort_order, profile_mod_id",
    )?;
    let rows = statement.query_map(params![profile_id, group_id], |row| row.get::<_, String>(0))?;
    let ids = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    update_profile_mod_order(transaction, &ids, now)
}

fn update_profile_mod_order(
    transaction: &Transaction<'_>,
    ids: &[String],
    now: i64,
) -> Result<(), LibraryError> {
    for (index, id) in ids.iter().enumerate() {
        transaction.execute(
            "UPDATE library_profile_mods SET sort_order = ?2, updated_at_ms = ?3
             WHERE profile_mod_id = ?1",
            params![id, index as u64, now],
        )?;
    }
    Ok(())
}

fn query_mod_summary(
    connection: &rusqlite::Connection,
    profile_id: &str,
    profile_mod_id: &str,
) -> Result<ModSummary, LibraryError> {
    connection
        .query_row(
            "SELECT pm.profile_mod_id, pm.incarnation_id, m.mod_id, m.source_id,
                    m.remote_game_key, m.remote_mod_id, m.display_name, m.revision,
                    pm.enabled, pm.sort_order, pm.group_id, pm.version_selection_kind,
                    pm.selected_version_id, selected.remote_file_id, binding.version_id,
                    m.tags_json, m.approval_status,
                    (SELECT CASE WHEN count(*) = 1 THEN NULLIF(NULLIF(max(version_id), '-'), 'legacy-unversioned') END
                     FROM library_mod_versions WHERE mod_id = m.mod_id AND current = 1)
             FROM library_profile_mods pm
             JOIN library_mods m ON m.mod_id = pm.mod_id
             LEFT JOIN library_mod_versions selected
               ON selected.mod_id = pm.mod_id AND selected.version_id = pm.selected_version_id
             LEFT JOIN library_profile_content_bindings binding
               ON binding.profile_mod_id = pm.profile_mod_id
             WHERE pm.profile_id = ?1 AND pm.profile_mod_id = ?2",
            params![profile_id, profile_mod_id],
            map_mod_summary,
        )
        .optional()?
        .ok_or_else(|| LibraryError::NotFound(profile_mod_id.to_owned()))
}

fn map_mod_summary(row: &rusqlite::Row<'_>) -> Result<ModSummary, rusqlite::Error> {
    let kind: String = row.get(11)?;
    let selected_version_id: Option<String> = row.get(12)?;
    let tags = row.get_ref(15)?.as_str().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(15, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let tags = serde_json::from_str(tags).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(15, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(ModSummary {
        profile_mod_id: row.get(0)?,
        incarnation_id: row.get(1)?,
        mod_id: row.get(2)?,
        r#mod: ModRef {
            source_id: row.get(3)?,
            remote_game_key: row.get(4)?,
            remote_mod_id: row.get(5)?,
            version_id: selected_version_id.clone(),
            remote_file_id: row.get(13)?,
        },
        display_name: row.get(6)?,
        tags,
        approval_status: row.get(16)?,
        current_version_id: row.get(17)?,
        entity_revision: row.get(7)?,
        enabled: row.get(8)?,
        sort_order: row.get(9)?,
        group_id: row.get(10)?,
        version_selection: VersionSelection {
            kind: if kind == "pinned" {
                VersionSelectionKind::Pinned
            } else {
                VersionSelectionKind::Auto
            },
            version_id: selected_version_id,
        },
        applied_version_id: row.get(14)?,
    })
}

pub(crate) fn replay_mutation<T: DeserializeOwned>(
    transaction: &Transaction<'_>,
    authorization: &LibraryAuthorization,
    scope: &str,
    idempotency_key: &str,
    inputs_hash: &str,
) -> Result<Option<T>, LibraryError> {
    if idempotency_key.trim().is_empty() {
        return Err(LibraryError::InvalidRecord(
            "idempotency key must not be empty".to_owned(),
        ));
    }
    let existing = transaction
        .query_row(
            "SELECT inputs_hash, result_json FROM library_mutation_results
             WHERE owner_scope = ?1 AND idempotency_scope = ?2 AND idempotency_key = ?3",
            params![authorization.owner_scope(), scope, idempotency_key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((stored_hash, result_json)) = existing else {
        return Ok(None);
    };
    if stored_hash != inputs_hash {
        return Err(LibraryError::IdempotencyConflict);
    }
    Ok(Some(serde_json::from_str(&result_json)?))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_mutation<T: Serialize>(
    transaction: &Transaction<'_>,
    authorization: &LibraryAuthorization,
    scope: &str,
    idempotency_key: &str,
    inputs_hash: &str,
    operation_id: &str,
    result: &T,
    now: i64,
) -> Result<(), LibraryError> {
    transaction.execute(
        "INSERT INTO library_mutation_results(
           owner_scope, idempotency_scope, idempotency_key, inputs_hash,
           result_json, operation_id, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            authorization.owner_scope(),
            scope,
            idempotency_key,
            inputs_hash,
            serde_json::to_string(result)?,
            operation_id,
            now
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_application_event(
    transaction: &Transaction<'_>,
    event_type: &str,
    payload: &Value,
    now: i64,
) -> Result<(), LibraryError> {
    insert_outbox(transaction, event_type, "application-events", payload, now)
}

fn insert_outbox(
    transaction: &Transaction<'_>,
    event_type: &str,
    destination: &str,
    payload: &Value,
    now: i64,
) -> Result<(), LibraryError> {
    transaction.execute(
        "INSERT INTO library_outbox(
           event_id, operation_id, event_type, destination, payload_json,
           delivery_state, delivery_ref, created_at_ms, delivered_at_ms
         ) VALUES (?1, NULL, ?2, ?3, ?4, 'pending', NULL, ?5, NULL)",
        params![
            Uuid::new_v4().to_string(),
            event_type,
            destination,
            serde_json::to_string(payload)?,
            now
        ],
    )?;
    Ok(())
}

fn validate_name(value: &str, field: &str) -> Result<(), LibraryError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 200 {
        return Err(LibraryError::InvalidRecord(format!(
            "{field} must contain 1 to 200 bytes"
        )));
    }
    Ok(())
}

fn validate_page_size(page_size: u64) -> Result<(), LibraryError> {
    if !(1..=100).contains(&page_size) {
        return Err(LibraryError::InvalidRecord(
            "page size must be between 1 and 100".to_owned(),
        ));
    }
    Ok(())
}

fn cursor_offset(
    encoded: Option<&str>,
    kind: &str,
    scope_hash: &str,
    snapshot: &str,
) -> Result<u64, LibraryError> {
    let Some(encoded) = encoded else {
        return Ok(0);
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| LibraryError::InvalidRecord("invalid pagination cursor".to_owned()))?;
    let cursor: PageCursor = serde_json::from_slice(&bytes)
        .map_err(|_| LibraryError::InvalidRecord("invalid pagination cursor".to_owned()))?;
    if cursor.kind != kind || cursor.scope_hash != scope_hash {
        return Err(LibraryError::InvalidRecord(
            "pagination cursor belongs to a different query".to_owned(),
        ));
    }
    if cursor.snapshot != snapshot {
        return Err(LibraryError::InvalidRecord(
            "pagination cursor snapshot is stale".to_owned(),
        ));
    }
    Ok(cursor.offset)
}

fn encode_cursor(
    kind: &str,
    scope_hash: &str,
    snapshot: &str,
    offset: u64,
) -> Result<String, LibraryError> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&PageCursor {
        kind: kind.to_owned(),
        scope_hash: scope_hash.to_owned(),
        snapshot: snapshot.to_owned(),
        offset,
    })?))
}

fn games_snapshot_fingerprint(
    transaction: &Transaction<'_>,
    game_id: Option<&str>,
) -> Result<String, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT g.game_id, g.updated_at_ms,
                COALESCE(MAX(i.updated_at_ms), 0), COUNT(i.installation_id)
         FROM library_games g
         LEFT JOIN library_installations i ON i.game_id = g.game_id
         WHERE (?1 IS NULL OR g.game_id = ?1)
         GROUP BY g.game_id ORDER BY g.game_id",
    )?;
    let rows = statement.query_map(params![game_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, u64>(3)?,
        ))
    })?;
    let mut hasher = Sha256::new();
    for row in rows {
        let (id, game_updated, installation_updated, count) = row?;
        hasher.update(id.as_bytes());
        hasher.update(game_updated.to_le_bytes());
        hasher.update(installation_updated.to_le_bytes());
        hasher.update(count.to_le_bytes());
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn profiles_snapshot_fingerprint(
    transaction: &Transaction<'_>,
    owner_scope: &str,
    game_id: &str,
) -> Result<String, LibraryError> {
    let mut statement = transaction.prepare(
        "SELECT profile_id, revision, content_revision, updated_at_ms
         FROM library_profiles
         WHERE owner_scope = ?1 AND game_id = ?2 AND deleted_at_ms IS NULL
         ORDER BY profile_id",
    )?;
    let rows = statement.query_map(params![owner_scope, game_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u64>(1)?,
            row.get::<_, u64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut hasher = Sha256::new();
    for row in rows {
        let (id, revision, content_revision, updated_at) = row?;
        hasher.update(id.as_bytes());
        hasher.update(revision.to_le_bytes());
        hasher.update(content_revision.to_le_bytes());
        hasher.update(updated_at.to_le_bytes());
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub(crate) fn hash_json(value: &Value) -> Result<String, LibraryError> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(value)?)
    ))
}

pub(crate) fn now_ms() -> Result<i64, LibraryError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| LibraryError::InvalidRecord(error.to_string()))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| LibraryError::InvalidRecord("timestamp overflow".to_owned()))
}
