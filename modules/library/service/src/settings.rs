use mintcat_sdk::{SettingsGetParams, SettingsResult, SettingsScope, SettingsUpdateParams};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::{LibraryAuthorization, LibraryError, OwnerStore, METADATA_WRITE_PERMISSION, READ_PERMISSION};
use crate::library_api::{record_mutation, replay_mutation};

type Result<T, E = LibraryError> = std::result::Result<T, E>;

fn scope_value(scope: &SettingsScope) -> &Value {
    match scope { SettingsScope::Value(value) | SettingsScope::Value2(value) => value }
}
fn scope_id(scope: &SettingsScope) -> Result<&str> {
    let value = scope_value(scope);
    match value["kind"].as_str() {
        Some("global") => Ok(crate::GLOBAL_SETTING_SCOPE),
        Some("game") => value["gameId"].as_str().filter(|value| !value.is_empty())
            .ok_or_else(|| LibraryError::InvalidRecord("game setting identity required".into())),
        _ => Err(LibraryError::InvalidRecord("unknown setting scope".into())),
    }
}
fn section_key(section: &str) -> Result<String> {
    if section.is_empty() || section.len() > 200 {
        return Err(LibraryError::InvalidRecord("setting section must contain 1 to 200 bytes".into()));
    }
    Ok(format!("application-settings:{section}"))
}
fn ensure_scope(connection: &rusqlite::Connection, scope: &SettingsScope, id: &str) -> Result<()> {
    if scope_value(scope)["kind"] == "game" {
        let exists: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM library_games WHERE game_id=?1)", [id], |row| row.get(0))?;
        if !exists { return Err(LibraryError::NotFound(format!("game {id}"))); }
    }
    Ok(())
}
fn load(connection: &rusqlite::Connection, id: &str, key: &str) -> Result<SettingsResult> {
    let encoded: Option<String> = connection.query_row(
        "SELECT value FROM library_settings WHERE scope_id=?1 AND name=?2", params![id,key], |row| row.get(0),
    ).optional()?;
    match encoded {
        Some(encoded) => Ok(serde_json::from_str(&encoded)?),
        None => Ok(SettingsResult { values: json!({}), revision: 0 }),
    }
}
impl OwnerStore {
    pub fn get_settings(&self, authorization: &LibraryAuthorization, request: &SettingsGetParams) -> Result<SettingsResult> {
        authorization.require(READ_PERMISSION)?;
        let id = scope_id(&request.scope)?;
        let key = section_key(&request.section)?;
        let connection = self.connection()?;
        ensure_scope(&connection, &request.scope, id)?;
        load(&connection, id, &key)
    }
    /// Schema is supplied only by the host from the current signed game descriptor.
    pub fn update_settings(&self, authorization: &LibraryAuthorization, key: &str, request: &SettingsUpdateParams, schema: Option<&Value>) -> Result<SettingsResult> {
        authorization.require(METADATA_WRITE_PERMISSION)?;
        if !request.values.is_object() { return Err(LibraryError::InvalidRecord("settings values must be an object".into())); }
        if let Some(schema) = schema {
            mintcat_sdk::validate_schema(schema, &request.values, "$.values")
                .map_err(|error| LibraryError::InvalidRecord(error.to_string()))?;
        }
        let id = scope_id(&request.scope)?;
        let section = section_key(&request.section)?;
        let hash = crate::digest::serialized_digest(request)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(result) = replay_mutation(&transaction, authorization, "settings.update", key, &hash)? {
            transaction.commit()?;
            return Ok(result);
        }
        ensure_scope(&transaction, &request.scope, id)?;
        let current = load(&transaction, id, &section)?;
        if current.revision != request.expected_revision {
            return Err(LibraryError::RevisionConflict { expected: request.expected_revision, actual: current.revision });
        }
        let revision = current.revision.checked_add(1).ok_or_else(|| LibraryError::InvalidRecord("settings revision overflow".into()))?;
        let result = SettingsResult { values: request.values.clone(), revision };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| LibraryError::InvalidRecord(error.to_string()))?.as_millis();
        let now = i64::try_from(now).map_err(|_| LibraryError::InvalidRecord("settings timestamp overflow".into()))?;
        transaction.execute(
            "INSERT INTO library_settings(scope_id,name,value,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?4)
             ON CONFLICT(scope_id,name) DO UPDATE SET value=excluded.value,updated_at_ms=excluded.updated_at_ms",
            params![id,section,serde_json::to_string(&result)?,now],
        )?;
        record_mutation(&transaction, authorization, "settings.update", key, &hash, key, &result, now)?;
        transaction.commit()?;
        Ok(result)
    }
}
