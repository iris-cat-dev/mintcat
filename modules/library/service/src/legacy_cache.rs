use std::{
    collections::HashMap,
    fs::{self, File, Metadata},
    io::Read,
    path::{Component, Path, PathBuf},
};

use mintcat_runtime_artifacts::ArtifactGrant;
use mintcat_sdk::ArtifactGrantDescriptor;
use rusqlite::{Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    ApplyOperationResult, CONTENT_WRITE_PERMISSION, LibraryAuthorization, LibraryError, OwnerStore,
    library_api::{
        ProfileRow, authorize_profile, bump_content_revision, ensure_content_available,
        ensure_revision, hash_json, insert_application_event, load_profile, now_ms,
    },
};

const RESTORE_SCOPE: &str = "library.restoreLegacyCache";
const AUTO_RESTORE_SCOPE: &str = "install.restoreLegacyCache";

/// An enabled, unbound membership and its explicitly recorded legacy cache file.
/// The file is an unverified local input, not evidence of remote provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyCacheCandidate {
    pub profile_mod_id: String,
    pub mod_id: String,
    pub version_id: String,
    pub path: PathBuf,
    pub file_size: u64,
    #[serde(skip)]
    fence: CandidateFence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidateFence {
    profile_incarnation_id: String,
    content_revision: u64,
    lease_epoch: u64,
    profile_mod_incarnation_id: String,
    mod_incarnation_id: String,
    mod_revision: u64,
    used_version: Option<String>,
    selection_kind: String,
    selected_version: Option<String>,
    recorded_file_size: u64,
    stored_digest: Option<String>,
    source_id: String,
    source_mod_id: String,
    remote_game_key: String,
    source_file_id: Option<String>,
}

impl LegacyCacheCandidate {
    /// Open the recorded input without traversing symlinks or Windows reparse points.
    pub fn open_read(&self) -> Result<File, LibraryError> {
        self.open_with_size(Some(self.file_size)).map(|(file, _)| file)
    }

    fn open_with_size(&self, expected_size: Option<u64>) -> Result<(File, u64), LibraryError> {
        if !self.path.extension().and_then(|extension| extension.to_str()).is_some_and(
            |ext| ext.eq_ignore_ascii_case("zip") || ext.eq_ignore_ascii_case("pak"),
        ) {
            return Err(self.invalid("recorded cache path is not a ZIP or PAK"));
        }
        let file = open_local_file(&self.path)
            .map_err(|error| self.invalid(format!("cannot open {}: {error}", self.path.display())))?;
        let size = file.metadata()?.len();
        if size == 0 {
            return Err(self.invalid("recorded cache file is empty"));
        }
        let recorded_size = self.fence.recorded_file_size;
        if recorded_size != 0 && size != recorded_size {
            return Err(self.invalid(format!("cache file is {size} bytes, expected {recorded_size}")));
        }
        if let Some(expected) = expected_size {
            if size != expected {
                return Err(self.invalid(format!("cache file is {size} bytes, expected {expected}")));
            }
        }
        Ok((file, size))
    }

    fn invalid(&self, reason: impl std::fmt::Display) -> LibraryError {
        LibraryError::InvalidRecord(format!("legacy cache for {}: {reason}", self.profile_mod_id))
    }
}

impl OwnerStore {
    pub(crate) fn legacy_cache_hints(
        &self,
        authorization: &LibraryAuthorization,
        profile_id: &str,
    ) -> Result<Vec<String>, LibraryError> {
        authorization.require(crate::READ_PERMISSION)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let profile = load_profile(&transaction, profile_id)?;
        authorize_profile(authorization, &profile)?;
        let (_, candidates) = load_candidates(&transaction, &profile, false)?;
        Ok(candidates.into_iter().map(|item| item.profile_mod_id).collect())
    }

    pub(crate) fn previous_legacy_cache_restore(
        &self,
        authorization: &LibraryAuthorization,
        profile_id: &str,
        expected_content_revision: u64,
        key: &str,
    ) -> Result<Option<ApplyOperationResult>, LibraryError> {
        authorization.require(CONTENT_WRITE_PERMISSION)?;
        let connection = self.connection()?;
        previous_restore(&connection, authorization, profile_id, expected_content_revision, key)
    }

    pub(crate) fn legacy_cache_candidates(
        &self,
        authorization: &LibraryAuthorization,
        profile_id: &str,
        expected_content_revision: u64,
        strict: bool,
    ) -> Result<Vec<LegacyCacheCandidate>, LibraryError> {
        authorization.require(CONTENT_WRITE_PERMISSION)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let profile = check_target(&transaction, authorization, profile_id, expected_content_revision)?;
        let (_, mut candidates) = load_candidates(&transaction, &profile, strict)?;
        if strict {
            for candidate in &mut candidates {
                candidate.file_size = candidate.open_with_size(None)?.1;
            }
        } else {
            candidates.retain_mut(|candidate| match candidate.open_with_size(None) {
                Ok((_, size)) => {
                    candidate.file_size = size;
                    true
                }
                Err(_) => false,
            });
        }
        transaction.commit()?;
        Ok(candidates)
    }

    pub(crate) fn restore_legacy_cache(
        &self,
        authorization: &LibraryAuthorization,
        profile_id: &str,
        expected_content_revision: u64,
        inputs: &[(LegacyCacheCandidate, ArtifactGrant)],
        artifacts_dir: &Path,
        key: Option<&str>,
    ) -> Result<ApplyOperationResult, LibraryError> {
        authorization.require(CONTENT_WRITE_PERMISSION)?;
        self.ensure_writable()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(key) = key {
            if let Some(result) = previous_restore(&transaction, authorization, profile_id, expected_content_revision, key)? {
                return Ok(result);
            }
        }
        let profile = check_target(&transaction, authorization, profile_id, expected_content_revision)?;
        let (lease_epoch, candidates) = load_candidates(&transaction, &profile, key.is_none())?;

        let mut grants = HashMap::with_capacity(inputs.len());
        for (candidate, grant) in inputs {
            if grants.insert(candidate.profile_mod_id.as_str(), (candidate, grant)).is_some() {
                return Err(candidate.invalid("duplicate membership"));
            }
        }
        if key.is_none() && grants.len() != candidates.len() {
            return Err(LibraryError::InvalidRecord(format!(
                "legacy cache restore requires all {} unbound memberships, received {}",
                candidates.len(), grants.len()
            )));
        }
        if inputs.is_empty() {
            return Ok(ApplyOperationResult {
                operation_id: String::new(),
                profile_id: profile_id.to_owned(),
                content_revision: profile.content_revision,
                artifact_digests: Vec::new(),
                applied: false,
            });
        }

        let mut verified = Vec::with_capacity(candidates.len());
        let blob_dir = artifacts_dir.join("blobs").join("sha256");
        for mut current in candidates {
            let Some((candidate, grant)) = grants.remove(current.profile_mod_id.as_str()) else {
                continue;
            };
            // The current size is verified from the recorded file, not trusted from the caller.
            let (source, size) = current.open_with_size(None)?;
            current.file_size = size;
            if &current != candidate {
                return Err(LibraryError::StaleTarget(current.profile_mod_id));
            }
            verify_grant(&current, source, grant, &blob_dir)?;
            verified.push((candidate, &grant.descriptor));
        }
        if !grants.is_empty() {
            return Err(LibraryError::StaleTarget(profile_id.to_owned()));
        }

        let now = now_ms()?;
        let operation_id = Uuid::new_v4().to_string();
        // This UUID also identifies the offline owner generation: no daemon job owns the lease.
        let fence_epoch = lease_epoch.checked_add(1).ok_or_else(|| {
            LibraryError::InvalidRecord("profile lease epoch overflow".to_owned())
        })?;
        let inputs_hash = hash_json(&json!({
            "profileId": profile_id,
            "expectedContentRevision": expected_content_revision,
            "inputs": verified.iter().map(|(candidate, grant)| {
                (&candidate.profile_mod_id, &candidate.version_id, &grant.digest, grant.size)
            }).collect::<Vec<_>>(),
        }))?;
        let fenced = transaction.execute(
            "UPDATE library_profiles SET lease_epoch = ?2
             WHERE profile_id = ?1 AND lease_epoch = ?3 AND incarnation_id = ?4
               AND content_revision = ?5 AND deleted_at_ms IS NULL",
            params![profile_id, fence_epoch, lease_epoch, profile.incarnation_id, expected_content_revision],
        )?;
        if fenced != 1 {
            return Err(LibraryError::StaleTarget(profile_id.to_owned()));
        }
        let content_revision = bump_content_revision(&transaction, profile_id, now)?;
        let result = ApplyOperationResult {
            operation_id: operation_id.clone(),
            profile_id: profile_id.to_owned(),
            content_revision,
            artifact_digests: verified.iter().map(|(_, grant)| grant.digest.clone()).collect(),
            applied: true,
        };
        transaction.execute(
            "INSERT INTO library_download_operations(
                operation_id, owner_scope, idempotency_scope, idempotency_key, inputs_hash,
                profile_id, mode, expected_content_revision, target_incarnation_id,
                owner_generation, fence_epoch, state, result_json, created_at_ms, updated_at_ms, completed_at_ms
             ) VALUES (?1, ?2, ?3, ?11, ?4, ?5, 'prepare-and-apply', ?6, ?7,
                       ?1, ?8, 'succeeded', ?9, ?10, ?10, ?10)",
            params![operation_id, authorization.owner_scope(),
                    if key.is_some() { AUTO_RESTORE_SCOPE } else { RESTORE_SCOPE }, inputs_hash,
                    profile_id, expected_content_revision, profile.incarnation_id, fence_epoch,
                    serde_json::to_string(&result)?, now, key.unwrap_or(&operation_id)],
        )?;
        transaction.execute(
            "INSERT INTO library_operation_leases(
                resource_key, operation_id, fence_epoch, owner_generation, state,
                acquired_at_ms, heartbeat_at_ms, released_at_ms
             ) VALUES (?1, ?2, ?3, ?2, 'released', ?4, ?4, ?4)
             ON CONFLICT(resource_key) DO UPDATE SET operation_id = excluded.operation_id,
                fence_epoch = excluded.fence_epoch, owner_generation = excluded.owner_generation,
                state = excluded.state, acquired_at_ms = excluded.acquired_at_ms,
                heartbeat_at_ms = excluded.heartbeat_at_ms, released_at_ms = excluded.released_at_ms",
            params![format!("profile-content:{profile_id}"), operation_id, fence_epoch, now],
        )?;
        for (candidate, descriptor) in verified {
            bind_candidate(&transaction, &operation_id, candidate, descriptor, now)?;
        }
        insert_application_event(
            &transaction, "profile.content.changed", &serde_json::to_value(&result)?, now,
        )?;
        // Fence legacy import rollback before publishing any owner data changes.
        crate::import_outcome::advance(self.database_path())?;
        transaction.commit()?;
        Ok(result)
    }
}

fn previous_restore(
    connection: &rusqlite::Connection,
    authorization: &LibraryAuthorization,
    profile_id: &str,
    expected_content_revision: u64,
    key: &str,
) -> Result<Option<ApplyOperationResult>, LibraryError> {
    use rusqlite::OptionalExtension;
    let record = connection.query_row(
        "SELECT profile_id, expected_content_revision, result_json
         FROM library_download_operations
         WHERE owner_scope = ?1 AND idempotency_scope = ?2 AND idempotency_key = ?3",
        params![authorization.owner_scope(), AUTO_RESTORE_SCOPE, key],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?, row.get::<_, String>(2)?)),
    ).optional()?;
    record.map(|(target, revision, result)| {
        if target != profile_id || revision != expected_content_revision {
            return Err(LibraryError::InvalidRecord("legacy cache restore key reused for different inputs".into()));
        }
        Ok(serde_json::from_str(&result)?)
    }).transpose()
}

fn check_target(
    transaction: &Transaction<'_>,
    authorization: &LibraryAuthorization,
    profile_id: &str,
    expected_content_revision: u64,
) -> Result<ProfileRow, LibraryError> {
    let profile = load_profile(transaction, profile_id)?;
    authorize_profile(authorization, &profile)?;
    ensure_revision(expected_content_revision, profile.content_revision)?;
    ensure_content_available(transaction, profile_id)?;
    Ok(profile)
}

fn load_candidates(
    transaction: &Transaction<'_>,
    profile: &ProfileRow,
    strict: bool,
) -> Result<(u64, Vec<LegacyCacheCandidate>), LibraryError> {
    let lease_epoch = transaction.query_row(
        "SELECT lease_epoch FROM library_profiles WHERE profile_id = ?1",
        [&profile.profile_id], |row| row.get::<_, u64>(0),
    )?;
    let mut statement = transaction.prepare(
        "SELECT p.profile_mod_id, p.mod_id, p.incarnation_id, p.used_version,
                p.version_selection_kind, p.selected_version_id, m.game_id, m.incarnation_id,
                m.revision, m.source_id, m.remote_mod_id, m.remote_game_key,
                COALESCE(v.version_id, ''), COALESCE(v.availability, ''),
                COALESCE(v.legacy_location, ''), COALESCE(v.file_size, 0),
                v.artifact_digest, v.remote_file_id
         FROM library_profile_mods p
         JOIN library_mods m ON m.mod_id = p.mod_id
         LEFT JOIN library_mod_versions v ON v.mod_id = p.mod_id AND v.current = 1
         WHERE p.profile_id = ?1 AND p.enabled = 1
           AND NOT EXISTS (SELECT 1 FROM library_profile_content_bindings b
                           WHERE b.profile_mod_id = p.profile_mod_id)
         ORDER BY p.sort_order, p.profile_mod_id, v.version_id",
    )?;
    let mut rows = statement.query([&profile.profile_id])?;
    let mut candidates: Vec<LegacyCacheCandidate> = Vec::new();
    while let Some(row) = rows.next()? {
        let game_id: String = row.get(6)?;
        let availability: String = row.get(13)?;
        let location: String = row.get(14)?;
        let candidate = LegacyCacheCandidate {
            profile_mod_id: row.get(0)?,
            mod_id: row.get(1)?,
            version_id: row.get(12)?,
            path: PathBuf::from(&location),
            file_size: 0,
            fence: CandidateFence {
                profile_incarnation_id: profile.incarnation_id.clone(),
                content_revision: profile.content_revision,
                lease_epoch,
                profile_mod_incarnation_id: row.get(2)?,
                used_version: row.get(3)?,
                selection_kind: row.get(4)?,
                selected_version: row.get(5)?,
                mod_incarnation_id: row.get(7)?,
                mod_revision: row.get(8)?,
                source_id: row.get(9)?,
                source_mod_id: row.get(10)?,
                remote_game_key: row.get(11)?,
                recorded_file_size: row.get(15)?,
                stored_digest: row.get(16)?,
                source_file_id: row.get(17)?,
            },
        };
        if game_id != profile.game_id || candidate.version_id.trim().is_empty()
            || availability != "legacy-unverified" || location.trim().is_empty()
        {
            if !strict { continue; }
            return Err(candidate.invalid("no current legacy-unverified version with a recorded path"));
        }
        if candidates.last().is_some_and(|last| last.profile_mod_id == candidate.profile_mod_id) {
            return Err(candidate.invalid("multiple current version definitions"));
        }
        let fence = &candidate.fence;
        let version = Some(candidate.version_id.as_str());
        if (fence.used_version.is_some() && fence.used_version.as_deref() != version)
            || (fence.selection_kind == "pinned" && fence.selected_version.as_deref() != version)
        {
            if !strict { continue; }
            return Err(candidate.invalid("current cache version does not match the used or pinned version"));
        }
        candidates.push(candidate);
    }
    Ok((lease_epoch, candidates))
}

fn verify_grant(
    candidate: &LegacyCacheCandidate,
    mut source: File,
    grant: &ArtifactGrant,
    blob_dir: &Path,
) -> Result<(), LibraryError> {
    let descriptor = &grant.descriptor;
    let unverified = || LibraryError::ArtifactNotVerified(candidate.profile_mod_id.clone());
    let digest_hex = descriptor.digest.strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| {
            byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
        }))
        .ok_or_else(unverified)?;
    let blob_path = blob_dir.join(digest_hex);
    if Path::new(&descriptor.path) != blob_path.as_path() || descriptor.size != candidate.file_size
        || candidate.fence.stored_digest.as_ref().is_some_and(|digest| digest != &descriptor.digest)
    {
        return Err(unverified());
    }
    let metadata = fs::symlink_metadata(&blob_path).map_err(|_| unverified())?;
    if is_link(&metadata) || !metadata.is_file() || metadata.len() != descriptor.size {
        return Err(unverified());
    }
    let mut blob = grant.open_read().map_err(|_| unverified())?;
    // Authenticate the published bytes and prove they equal the recorded local file,
    // even for two same-sized files with different contents. Hash the matching bytes once.
    let mut hash = Sha256::new();
    let mut source_buffer = [0; 32 * 1024];
    let mut blob_buffer = [0; 32 * 1024];
    let mut remaining = descriptor.size;
    while remaining != 0 {
        let len = remaining.min(source_buffer.len() as u64) as usize;
        source.read_exact(&mut source_buffer[..len]).map_err(|_| unverified())?;
        blob.read_exact(&mut blob_buffer[..len]).map_err(|_| unverified())?;
        if source_buffer[..len] != blob_buffer[..len] {
            return Err(unverified());
        }
        hash.update(&source_buffer[..len]);
        remaining -= len as u64;
    }
    if source.read(&mut source_buffer[..1]).map_err(|_| unverified())? != 0
        || blob.read(&mut blob_buffer[..1]).map_err(|_| unverified())? != 0
        || format!("sha256:{:x}", hash.finalize()) != descriptor.digest
    {
        return Err(unverified());
    }
    Ok(())
}

fn bind_candidate(
    transaction: &Transaction<'_>,
    operation_id: &str,
    candidate: &LegacyCacheCandidate,
    descriptor: &ArtifactGrantDescriptor,
    now: i64,
) -> Result<(), LibraryError> {
    transaction.execute(
        "INSERT INTO library_content_refs(digest, size, verified, grant_id, created_at_ms)
         VALUES (?1, ?2, 1, ?3, ?4) ON CONFLICT(digest) DO NOTHING",
        params![descriptor.digest, descriptor.size, descriptor.grant_id, now],
    )?;
    let stored_size = transaction.query_row(
        "SELECT size FROM library_content_refs WHERE digest = ?1 AND verified = 1",
        [&descriptor.digest], |row| row.get::<_, u64>(0),
    )?;
    if stored_size != descriptor.size {
        return Err(LibraryError::ArtifactNotVerified(candidate.profile_mod_id.clone()));
    }
    let fence = &candidate.fence;
    let version = transaction.execute(
        "UPDATE library_mod_versions SET artifact_digest = ?3
         WHERE mod_id = ?1 AND version_id = ?2 AND current = 1
           AND availability = 'legacy-unverified' AND (artifact_digest IS NULL OR artifact_digest = ?3)",
        params![candidate.mod_id, candidate.version_id, descriptor.digest],
    )?;
    let membership = transaction.execute(
        "UPDATE library_profile_mods SET used_version = ?2
         WHERE profile_mod_id = ?1 AND incarnation_id = ?3 AND enabled = 1",
        params![candidate.profile_mod_id, candidate.version_id, fence.profile_mod_incarnation_id],
    )?;
    if version != 1 || membership != 1 {
        return Err(LibraryError::StaleTarget(candidate.profile_mod_id.clone()));
    }
    transaction.execute(
        "INSERT INTO library_download_operation_items(
            operation_id, item_id, profile_mod_id, profile_mod_incarnation_id, mod_id, source_id,
            source_mod_id, remote_game_key, source_file_id, resolved_version, expected_digest,
            artifact_digest, state
         ) VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'verified')",
        params![operation_id, candidate.profile_mod_id, fence.profile_mod_incarnation_id,
                candidate.mod_id, fence.source_id, fence.source_mod_id, fence.remote_game_key,
                fence.source_file_id, candidate.version_id, fence.stored_digest, descriptor.digest],
    )?;
    transaction.execute(
        "INSERT INTO library_profile_content_bindings(
            profile_mod_id, version_id, artifact_digest, operation_id, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![candidate.profile_mod_id, candidate.version_id, descriptor.digest, operation_id, now],
    )?;
    Ok(())
}

fn invalid_local_path() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "expected an absolute local regular file with no symlinks")
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Reject all reparse points (including junctions), not just recognized symlinks.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

#[cfg(unix)]
fn open_local_file(path: &Path) -> std::io::Result<File> {
    use std::{ffi::CString, os::{fd::{AsRawFd, FromRawFd}, unix::ffi::OsStrExt}};

    let mut components = path.components().peekable();
    if components.next() != Some(Component::RootDir) || components.peek().is_none() {
        return Err(invalid_local_path());
    }
    let mut directory = File::open("/")?;
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else { return Err(invalid_local_path()); };
        let name = CString::new(name.as_bytes()).map_err(|_| invalid_local_path())?;
        let last = components.peek().is_none();
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK
            | if last { 0 } else { libc::O_DIRECTORY };
        // The checked directory handle anchors each lookup even if an ancestor is renamed.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let opened = unsafe { File::from_raw_fd(fd) };
        if last {
            return opened.metadata()?.is_file().then_some(opened).ok_or_else(invalid_local_path);
        }
        directory = opened;
    }
    Err(invalid_local_path())
}

#[cfg(windows)]
fn open_local_file(path: &Path) -> std::io::Result<File> {
    use std::{fs::OpenOptions, os::windows::{ffi::OsStrExt, fs::OpenOptionsExt}, path::Prefix};

    let mut components = path.components().peekable();
    let Some(Component::Prefix(prefix)) = components.next() else { return Err(invalid_local_path()); };
    if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        || components.next() != Some(Component::RootDir) || components.peek().is_none()
    {
        return Err(invalid_local_path());
    }
    const FILE_SHARE_READ: u32 = 1;
    const FILE_LIST_DIRECTORY: u32 = 1;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let mut current = PathBuf::from(prefix.as_os_str());
    current.push(r"\");
    // Without delete/write sharing, checked ancestors cannot be replaced or turned into
    // reparse points while subsequent components are opened.
    let open_dir = |path: &Path| -> std::io::Result<File> {
        let dir = OpenOptions::new().access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS).open(path)?;
        let metadata = dir.metadata()?;
        if is_link(&metadata) || !metadata.is_dir() {
            return Err(invalid_local_path());
        }
        Ok(dir)
    };
    let mut directories = vec![open_dir(&current)?];
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else { return Err(invalid_local_path()); };
        if name.encode_wide().any(|unit| unit == u16::from(b':')) {
            return Err(invalid_local_path());
        }
        current.push(name);
        if components.peek().is_some() {
            directories.push(open_dir(&current)?);
            continue;
        }
        let file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS).open(&current)?;
        let metadata = file.metadata()?;
        if is_link(&metadata) || !metadata.is_file() {
            return Err(invalid_local_path());
        }
        return Ok(file);
    }
    Err(invalid_local_path())
}

#[cfg(not(any(unix, windows)))]
fn open_local_file(_: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "legacy cache requires a no-follow file opener"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mintcat_runtime_artifacts::{ArtifactStore, TrustStore};
    use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};

    #[test]
    fn restore_is_atomic_and_preserves_legacy_selection_and_definitions() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let layout = RuntimePaths::new(root.join("state")).unwrap()
            .initialize(RuntimeEnvironment::Test).unwrap();
        let store = OwnerStore::open(&layout).unwrap();
        let artifacts = ArtifactStore::from_layout(
            &layout, RuntimeEnvironment::Test, "test-target", "1", TrustStore::default(),
        ).unwrap();
        let auto = root.join("automatic.pak");
        let pinned = root.join("pinned.zip");
        fs::write(&auto, b"auto").unwrap();
        fs::write(&pinned, b"keep").unwrap();
        {
            let connection = store.connection().unwrap();
            connection.execute_batch(
                "INSERT INTO library_games VALUES ('game','game','Game','',1,0,0);
                 INSERT INTO library_profiles(
                    profile_id,game_id,owner_scope,name,display_name,active,description,revision,
                    content_revision,incarnation_id,last_used_at_ms,created_at_ms,updated_at_ms
                 ) VALUES ('profile','game','owner','default','Default',1,'',1,7,'profile-inc',0,0,0);",
            ).unwrap();
            for (id, version, path, size, order, selection, used) in [
                ("auto", "legacy-unversioned", Some(auto.as_path()), 0, 0, "auto", None),
                ("pinned", "exact", Some(pinned.as_path()), 4, 1, "pinned", Some("exact")),
                ("disabled", "other", None, 0, 2, "auto", None),
            ] {
                connection.execute(
                    "INSERT INTO library_mods(
                        mod_id,game_id,source_id,remote_game_key,remote_mod_id,name_id,display_name,
                        original_name,url,tags_json,approval_status,last_update_at_ms,online_update_at_ms,
                        online_available,local_missing,created_at_ms,updated_at_ms,incarnation_id
                     ) VALUES (?1,'game','local','game',?1,?1,?1,?1,'','[]','',0,0,0,0,0,0,?1)",
                    [id],
                ).unwrap();
                connection.execute(
                    "INSERT INTO library_mod_versions(
                        mod_id,version_id,legacy_location,file_size,availability,current,
                        created_at_ms,updated_at_ms
                     ) VALUES (?1,?2,?3,?4,'legacy-unverified',1,1,2)",
                    params![id, version, path.map(|path| path.to_str().unwrap()), size],
                ).unwrap();
                connection.execute(
                    "INSERT INTO library_profile_mods(
                        profile_mod_id,profile_id,mod_id,incarnation_id,sort_order,enabled,used_version,
                        created_at_ms,updated_at_ms,version_selection_kind,selected_version_id
                     ) VALUES (?1,'profile',?1,?1,?2,?3,?4,1,2,?5,?4)",
                    params![id, order, id != "disabled", used, selection],
                ).unwrap();
            }
        }
        let auth = LibraryAuthorization::new("owner", [CONTENT_WRITE_PERMISSION]);
        let stranger = LibraryAuthorization::new("other", [CONTENT_WRITE_PERMISSION]);
        assert!(matches!(
            store.legacy_cache_candidates(&stranger, "profile", 7, true),
            Err(LibraryError::PermissionDenied(_))
        ));
        assert!(matches!(
            store.legacy_cache_candidates(&auth, "profile", 6, true),
            Err(LibraryError::RevisionConflict { .. })
        ));
        let candidates = store.legacy_cache_candidates(&auth, "profile", 7, true).unwrap();
        assert_eq!(candidates.iter().map(|candidate| candidate.file_size).collect::<Vec<_>>(), [4, 4]);
        let before = store.snapshot().unwrap();
        let publish = |mut bytes: &[u8]| artifacts.publish_blob_reader(&mut bytes, None).unwrap();
        let automatic_artifact = publish(b"auto");
        let wrong_artifact = publish(b"swap");
        let pinned_artifact = publish(b"keep");
        let attempt = |second| {
            [
                (candidates[0].clone(), artifacts.grant_blob(&automatic_artifact).unwrap()),
                (candidates[1].clone(), artifacts.grant_blob(second).unwrap()),
            ]
        };
        let wrong = attempt(&wrong_artifact);
        assert!(matches!(
            store.restore_legacy_cache(&auth, "profile", 7, &wrong[..1], &layout.artifacts_dir, None),
            Err(LibraryError::InvalidRecord(_))
        ));
        assert!(matches!(
            store.restore_legacy_cache(&auth, "profile", 7, &wrong, &layout.artifacts_dir, None),
            Err(LibraryError::ArtifactNotVerified(_))
        ));
        assert_eq!(store.snapshot().unwrap(), before);
        assert_eq!(crate::import_outcome::epoch(store.database_path()).unwrap(), 0);
        drop(wrong);

        let valid = attempt(&pinned_artifact);
        assert_eq!(store.restore_legacy_cache(&auth, "profile", 7, &valid, &layout.artifacts_dir, None).unwrap().content_revision, 8);
        let after = store.snapshot().unwrap();
        let mut expected_memberships = before.profile_mods;
        expected_memberships.iter_mut().find(|item| item.profile_mod_id == "auto").unwrap()
            .used_version = Some("legacy-unversioned".into());
        assert_eq!(after.profile_mods, expected_memberships);
        assert_eq!(after.mods, before.mods);
        let mut expected_versions = before.versions;
        for (candidate, grant) in &valid {
            expected_versions.iter_mut().find(|v| v.mod_id == candidate.mod_id).unwrap()
                .artifact_digest = Some(grant.descriptor.digest.clone());
        }
        assert_eq!(after.versions, expected_versions);
        assert_eq!(after.profile_content_bindings.len(), 2);
        assert_eq!(after.download_operations.len(), 1);
        assert_eq!(after.download_operation_items.len(), 2);
        assert_eq!(after.operation_leases[0].state, "released");
        assert_eq!(after.profiles[0].lease_epoch, 1);
        assert!(store.legacy_cache_candidates(&auth, "profile", 8, true).unwrap().is_empty());
        assert!(store.restore_legacy_cache(&auth, "profile", 8, &valid, &layout.artifacts_dir, None).is_err());
        assert_eq!(store.snapshot().unwrap(), after);
    }
}
