use std::{fs::{self, File, OpenOptions}, io::Write, path::Path};
use crate::*;

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new(); options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    options
}
pub(super) fn writer_lock(path: &Path) -> Result<File, AgentError> {
    let parent = path.parent().ok_or(AgentError::Storage)?;
    fs::create_dir_all(parent).map_err(|_| AgentError::Storage)?;
    let lock_path = path.with_extension("writer-lock");
    if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() { return Err(AgentError::Storage); }
    }
    let mut options = OpenOptions::new(); options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let lock = options.open(lock_path).map_err(|_| AgentError::Storage)?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| AgentError::Busy)?;
    Ok(lock)
}
pub(super) fn prepare_database(path: &Path) -> Result<bool, AgentError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() { return Err(AgentError::Storage); }
            let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| AgentError::Storage)?;
            let version: u64 = db.query_row("PRAGMA user_version", [], |row| row.get(0)).map_err(|_| AgentError::Storage)?;
            let health: String = db.query_row("PRAGMA quick_check", [], |row| row.get(0)).map_err(|_| AgentError::Storage)?;
            if version > 1 || health != "ok" { return Err(AgentError::Storage); }
            Ok(version == 0)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            private_options().open(path).map_err(|_| AgentError::Storage)?;
            Ok(true)
        }
        Err(_) => Err(AgentError::Storage),
    }
}
fn epoch(path: &Path) -> Result<u64, AgentError> {
    let path = path.with_extension("write-epoch");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => fs::read_to_string(path).map_err(|_| AgentError::Storage)?.parse().map_err(|_| AgentError::Storage),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        _ => Err(AgentError::Storage),
    }
}
pub(super) fn advance_epoch(path: &Path) -> Result<(), AgentError> {
    let next = epoch(path)?.checked_add(1).ok_or(AgentError::Storage)?;
    let parent = path.parent().ok_or(AgentError::Storage)?;
    let temporary = parent.join(format!(".agent-epoch-{}", Uuid::new_v4()));
    let mut file = private_options().open(&temporary).map_err(|_| AgentError::Storage)?;
    write!(file, "{next}").map_err(|_| AgentError::Storage)?;
    file.sync_all().map_err(|_| AgentError::Storage)?; drop(file);
    fs::rename(temporary, path.with_extension("write-epoch")).map_err(|_| AgentError::Storage)?;
    // Windows cannot open a directory with File::open; the epoch file itself is already synced.
    #[cfg(unix)]
    File::open(parent).and_then(|file| file.sync_all()).map_err(|_| AgentError::Storage)?;
    Ok(())
}
impl AgentSessions {
    pub fn database_path(&self) -> &Path { &self.path }
    pub fn write_epoch(&self) -> Result<u64, AgentError> { epoch(&self.path) }
    pub(super) fn advance_epoch(&self) -> Result<(), AgentError> { advance_epoch(&self.path) }
    pub fn portable_snapshot(&self) -> Result<Vec<AgentSession>, AgentError> {
        let active = self.active.0.lock().map_err(|_| AgentError::Storage)?;
        if active.is_some() { return Err(AgentError::Busy); }
        let db = self.db.lock().map_err(|_| AgentError::Storage)?;
        let mut query = db.prepare("SELECT data FROM agent_sessions ORDER BY session_id").map_err(|_| AgentError::Storage)?;
        let rows = query.query_map([], |row| row.get::<_, String>(0)).map_err(|_| AgentError::Storage)?;
        let mut sessions = Vec::new();
        for row in rows {
            let mut stored: StoredSession = serde_json::from_str(&row.map_err(|_| AgentError::Storage)?).map_err(|_| AgentError::Storage)?;
            if stored.session.state == "sending" { return Err(AgentError::Busy); }
            stored.session.configuration.credential_ref = None;
            if !stored.session.bindings.is_empty() {
                stored.session.timeline.push(TimelineItem {
                    id: format!("imported-task-history:{}", stored.session.session_id), role: "tool".into(),
                    content: format!("Non-authoritative task history; these IDs do not authorize tasks on a restored machine: {}", serde_json::to_string(&stored.session.bindings).map_err(|_| AgentError::Storage)?), tool_name: None,
                });
                stored.session.bindings.clear();
            }
            sessions.push(stored.session);
        }
        Ok(sessions)
    }
    pub fn validate_portable(&self, sessions: &[AgentSession]) -> Result<(), AgentError> {
        let mut ids = std::collections::BTreeSet::new();
        for session in sessions {
            if !ids.insert(&session.session_id) || session.revision == 0 || !["idle", "failed"].contains(&session.state.as_str()) || session.configuration.credential_ref.is_some() || !session.bindings.is_empty() { return Err(AgentError::InvalidRequest); }
            validate_configuration_target(&session.configuration, &session.target)?;
        }
        Ok(())
    }
    pub fn import_empty_portable(&self, sessions: &[AgentSession], expected_epoch: u64) -> Result<(), AgentError> {
        self.validate_portable(sessions)?;
        let active = self.active.0.lock().map_err(|_| AgentError::Storage)?;
        if active.is_some() { return Err(AgentError::Busy); }
        let mut db = self.db.lock().map_err(|_| AgentError::Storage)?;
        let tx = db.transaction().map_err(|_| AgentError::Storage)?;
        let nonempty: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_sessions) OR EXISTS(SELECT 1 FROM agent_operations)", [], |row| row.get(0)).map_err(|_| AgentError::Storage)?;
        if nonempty || self.write_epoch()? != expected_epoch { return Err(AgentError::Busy); }
        self.advance_epoch()?;
        for session in sessions {
            let history = session.timeline.iter().filter(|item| ["user", "assistant"].contains(&item.role.as_str()) && item.tool_name.is_none()).map(|item| json!({"role": item.role, "content": item.content})).collect();
            persist(&tx, &StoredSession { session: session.clone(), history })?;
        }
        tx.commit().map_err(|_| AgentError::Storage)
    }
}
