use super::*;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Snapshot {
    Missing,
    File { digest: String, mode: Option<u32> },
    Foreign(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Phase {
    Prepared,
    Quarantined,
    Restored,
    Verified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UninstallEntry {
    relative_path: String,
    before: Snapshot,
    desired: Snapshot,
    mutate: bool,
    staged_relative_path: String,
    saved_relative_path: String,
    phase: Phase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UninstallJournal {
    schema_version: u32,
    transaction_id: String,
    installation_id: String,
    owner_scope: String,
    root_path: String,
    root_identity: String,
    receipt_ids: Vec<String>,
    state: FileTransactionState,
    sequence: u64,
    commit_decided: bool,
    #[serde(default)]
    cancel_requested_during_commit: bool,
    entries: Vec<UninstallEntry>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl UninstallJournal {
    fn area(&self) -> PathBuf {
        PathBuf::from(&self.root_path).join(TARGET_TRANSACTION_DIR).join(&self.transaction_id)
    }

    fn mismatch(&self, path: &str, reason: &str) -> FileTransactionError {
        FileTransactionError::RecoveryRequired {
            transaction_id: self.transaction_id.clone(),
            reason: format!("{path}: {reason}"),
        }
    }

    fn report(&self) -> UninstallChainReport {
        let mut report = UninstallChainReport {
            processed_receipt_ids: self.receipt_ids.clone(),
            disposition: UninstallDisposition::Completed,
            restored: Vec::new(),
            removed: Vec::new(),
            preserved_modified: Vec::new(),
            missing: Vec::new(),
            foreign_files_untouched: true,
            cancel_requested_during_commit: self.cancel_requested_during_commit,
        };
        for entry in &self.entries {
            if entry.mutate {
                if entry.desired == Snapshot::Missing {
                    report.removed.push(entry.relative_path.clone());
                } else {
                    report.restored.push(entry.relative_path.clone());
                }
            } else if entry.before == Snapshot::Missing {
                report.missing.push(entry.relative_path.clone());
            } else {
                report.preserved_modified.push(entry.relative_path.clone());
            }
        }
        if !report.preserved_modified.is_empty() {
            report.disposition = UninstallDisposition::CompletedWithPreservedFiles;
        }
        report
    }
}

impl FileTransactionManager {
    pub fn has_unresolved(&self) -> Result<bool, FileTransactionError> {
        let connection = self.database.connection()?;
        Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_transactions WHERE state IN ('prepared', 'applying', 'recovery-required'))",
            [], |row| row.get(0),
        )?)
    }

    pub fn has_unresolved_for_installation(
        &self,
        installation_id: &str,
    ) -> Result<bool, FileTransactionError> {
        Ok(self.database.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_transactions WHERE installation_id=?1 AND state IN ('prepared', 'applying', 'recovery-required'))",
            [installation_id], |row| row.get(0),
        )?)
    }

    /// Reads the atomic committed decision, without replaying filesystem operations.
    /// Only the newest receipt identifies a chain; older chain members cannot replay it.
    pub fn committed_uninstall_result(
        &self,
        receipt_id: &str,
    ) -> Result<Option<UninstallChainReport>, FileTransactionError> {
        let connection = self.database.connection()?;
        let installation: Option<String> = connection.query_row(
            "SELECT installation_id FROM install_receipts WHERE receipt_id=?1",
            [receipt_id], |row| row.get(0),
        ).optional()?;
        let Some(installation) = installation else { return Ok(None); };
        let mut statement = connection.prepare(
            "SELECT journal_json FROM file_transactions WHERE installation_id=?1 AND kind='uninstall' AND state='committed' ORDER BY created_at_ms DESC, rowid DESC",
        )?;
        let rows = statement.query_map([installation], |row| row.get::<_, String>(0))?;
        for raw in rows {
            let journal: UninstallJournal = serde_json::from_str(&raw?)?;
            if journal.receipt_ids.first().map(String::as_str) != Some(receipt_id) { continue; }
            if journal.schema_version != 1 || !journal.commit_decided || journal.state != FileTransactionState::Committed {
                return Err(journal.mismatch("journal", "committed index has no authoritative uninstall decision"));
            }
            for member in &journal.receipt_ids {
                let inactive: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM install_receipts WHERE receipt_id=?1 AND state IN ('uninstalled','uninstalled-preserved'))",
                    [member], |row| row.get(0),
                )?;
                if !inactive { return Err(journal.mismatch(member, "committed chain receipt is not inactive")); }
            }
            return Ok(Some(journal.report()));
        }
        Ok(None)
    }

    pub fn uninstall_chain(
        &self,
        receipt_ids: &[String],
        process_guard: &dyn TargetProcessGuard,
        cancellation: &FileCancellation,
    ) -> Result<UninstallChainReport, FileTransactionError> {
        self.uninstall_chain_internal(receipt_ids, process_guard, cancellation, None, None)
    }

    pub(super) fn uninstall_chain_internal(
        &self,
        receipt_ids: &[String],
        process_guard: &dyn TargetProcessGuard,
        cancellation: &FileCancellation,
        crash_after_mutation: Option<usize>,
        cancel_after_mutation: Option<usize>,
    ) -> Result<UninstallChainReport, FileTransactionError> {
        let first_id = receipt_ids.first().ok_or_else(|| {
            FileTransactionError::InvalidRequest("entire newest-first active receipt chain is required".to_owned())
        })?;
        let first = self.receipt(first_id)?;
        let root = canonical_directory(Path::new(&first.root_path))?;
        let identity = physical_root_identity(&root)?;
        let _lock = self.acquire_root_lock(&identity)?;
        self.ensure_no_active_transaction(&identity)?;
        if identity != first.root_identity {
            return Err(FileTransactionError::RecoveryRequired {
                transaction_id: first.transaction_id.clone(),
                reason: "installation root identity changed".to_owned(),
            });
        }
        let receipts = self.active_receipts_for_installation(&first.installation_id)?;
        if receipts.iter().map(|receipt| &receipt.receipt_id).ne(receipt_ids.iter()) {
            return Err(FileTransactionError::InvalidRequest("receipt chain is not the exact newest-first active chain".to_owned()));
        }
        for receipt in &receipts {
            if receipt.installation_id != first.installation_id || receipt.owner_scope != first.owner_scope
                || receipt.root_identity != identity || physical_root_identity(Path::new(&receipt.root_path))? != identity {
                return Err(FileTransactionError::InvalidRequest("receipt chain ownership or physical root differs".to_owned()));
            }
        }
        let connection = self.database.connection()?;
        let foreign_owner: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM install_receipts WHERE root_identity = ?1 AND state = 'active' AND installation_id != ?2)",
            params![identity, first.installation_id], |row| row.get(0),
        )?;
        drop(connection);
        if foreign_owner {
            return Err(FileTransactionError::InvalidRequest("root has active receipts from another installation".to_owned()));
        }
        process_guard.ensure_stopped(&root).map_err(FileTransactionError::TargetRunning)?;
        let cancelled = || UninstallChainReport {
            processed_receipt_ids: Vec::new(), disposition: UninstallDisposition::Cancelled,
            restored: Vec::new(), removed: Vec::new(), preserved_modified: Vec::new(), missing: Vec::new(),
            foreign_files_untouched: true, cancel_requested_during_commit: false,
        };
        if cancellation.is_requested() { return Ok(cancelled()); }

        // Each path is processed once, from the current newest bytes to the oldest original.
        let mut targets: Vec<(&ReceiptFile, &ReceiptFile, &InstallReceipt)> = Vec::new();
        let mut by_path: HashMap<String, usize> = HashMap::new();
        for receipt in &receipts {
            let mut seen = HashSet::new();
            for file in &receipt.files {
                let relative = checked_relative_path(&file.relative_path)?;
                let key = path_identity_key(&relative);
                if !seen.insert(key.clone()) { return Err(FileTransactionError::DuplicateTarget(file.relative_path.clone())); }
                if let Some(&index) = by_path.get(&key) {
                    let (_, oldest, _) = targets[index];
                    if oldest.relative_path != file.relative_path || oldest.owner != file.owner {
                        return Err(FileTransactionError::DuplicateTarget(file.relative_path.clone()));
                    }
                    targets[index].1 = file;
                    targets[index].2 = receipt;
                } else {
                    by_path.insert(key, targets.len());
                    targets.push((file, file, receipt));
                }
            }
        }
        let transaction_id = Uuid::new_v4().to_string();
        let area = root.join(TARGET_TRANSACTION_DIR).join(&transaction_id);
        validate_existing_ancestors(&root, Path::new(&format!("{TARGET_TRANSACTION_DIR}/{transaction_id}/staged/file")))?;
        let mut pending = PendingArea::new(area.clone());
        create_dir_all(&area.join("staged"))?;
        create_dir_all(&area.join("saved"))?;
        ensure_same_filesystem(&root, &area)?;
        let mut required = 0_u64;
        let mut entries = Vec::with_capacity(targets.len());
        for (index, (newest, oldest, source_receipt)) in targets.iter().enumerate() {
            let relative = checked_relative_path(&newest.relative_path)?;
            validate_existing_ancestors(&root, &relative)?;
            let before = snapshot(&root.join(&relative))?;
            let mutate = matches!(&before, Snapshot::File { digest, .. } if digest == &newest.installed_digest);
            let staged_relative_path = format!("staged/{index:08}");
            let desired = if mutate {
                if let Some(digest) = &oldest.before_digest {
                    let backup_relative = oldest.backup_relative_path.as_ref().ok_or_else(|| {
                        FileTransactionError::InvalidRequest("original digest has no owned backup".to_owned())
                    })?;
                    let transaction_relative = checked_relative_path(&source_receipt.transaction_id)?;
                    let backup_relative = checked_relative_path(backup_relative)?;
                    let backup = root.join(TARGET_TRANSACTION_DIR).join(transaction_relative).join(backup_relative);
                    let backup_root = root.join(TARGET_TRANSACTION_DIR);
                    validate_existing_ancestors(&backup_root, backup.strip_prefix(&backup_root).map_err(|_| FileTransactionError::InvalidPath(backup.display().to_string()))?)?;
                    if !matches!(snapshot(&backup)?, Snapshot::File { digest: actual, .. } if actual == *digest) {
                        return Err(FileTransactionError::RecoveryRequired {
                            transaction_id: source_receipt.transaction_id.clone(),
                            reason: format!("owned backup for {} is missing or changed", oldest.relative_path),
                        });
                    }
                    let size = fs::metadata(&backup).map_err(|source| io_error(&backup, source))?.len();
                    required = required.checked_add(size).ok_or_else(|| FileTransactionError::InvalidRequest("staging size overflow".to_owned()))?;
                    let available = fs2::available_space(&root).map_err(|source| io_error(&root, source))?;
                    if available < required { return Err(FileTransactionError::InsufficientSpace { required, available }); }
                    let staged = area.join(&staged_relative_path);
                    let mut input = File::open(&backup).map_err(|source| io_error(&backup, source))?;
                    let (actual, _) = copy_and_hash(&mut input, &staged)?;
                    if actual != *digest { return Err(FileTransactionError::TargetMismatch { path: oldest.relative_path.clone(), reason: "owned backup changed during staging".to_owned() }); }
                    set_mode(&staged, oldest.original_mode)?;
                    sync_file(&staged)?;
                    Snapshot::File { digest: digest.clone(), mode: oldest.original_mode }
                } else { Snapshot::Missing }
            } else { before.clone() };
            entries.push(UninstallEntry { relative_path: newest.relative_path.clone(), before, desired, mutate,
                staged_relative_path, saved_relative_path: format!("saved/{index:08}"), phase: Phase::Prepared });
        }
        sync_directory(&area.join("staged"))?;
        sync_directory(&area.join("saved"))?;
        sync_directory(&area)?;
        sync_parent(&area)?;
        if cancellation.is_requested() { return Ok(cancelled()); }
        let now = now_ms()?;
        let mut journal = UninstallJournal { schema_version: 1, transaction_id, installation_id: first.installation_id,
            owner_scope: first.owner_scope, root_path: root.to_string_lossy().into_owned(), root_identity: identity,
            receipt_ids: receipt_ids.to_vec(), state: FileTransactionState::Prepared, sequence: 0, commit_decided: false,
            cancel_requested_during_commit: false, entries, created_at_ms: now, updated_at_ms: now };
        self.insert_uninstall_journal(&journal)?;
        pending.disarm();
        if cancellation.is_requested() {
            journal.state = FileTransactionState::RolledBack;
            self.persist_uninstall_journal(&mut journal)?;
            cleanup_dir(&area);
            return Ok(cancelled());
        }
        let result = self.apply_uninstall(&mut journal, cancellation, crash_after_mutation, cancel_after_mutation);
        if let Err(error) = result {
            #[cfg(test)]
            if matches!(error, FileTransactionError::SimulatedCrash) { return Err(error); }
            if journal.commit_decided {
                return Err(self.uninstall_recovery_error(&mut journal, error.to_string()));
            }
            return match self.rollback_uninstall(&mut journal) {
                Ok(()) => Err(FileTransactionError::CommitRolledBack { transaction_id: journal.transaction_id.clone(), reason: error.to_string() }),
                Err(rollback) => Err(self.uninstall_recovery_error(&mut journal, format!("{error}; rollback failed: {rollback}"))),
            };
        }
        let report = journal.report();
        // Directory removal is deliberately post-decision: never remove parents needed by rollback.
        for receipt in &receipts {
            for file in &receipt.files { remove_empty_dirs(&root, &file.created_parent_dirs); }
        }
        Ok(report)
    }

    fn apply_uninstall(&self, journal: &mut UninstallJournal, cancellation: &FileCancellation,
        crash: Option<usize>, cancel: Option<usize>) -> Result<(), FileTransactionError> {
        journal.state = FileTransactionState::Applying;
        self.persist_uninstall_journal(journal)?;
        let root = PathBuf::from(&journal.root_path);
        validate_uninstall_area(journal)?;
        let area = journal.area();
        let mut mutation = 0;
        for index in 0..journal.entries.len() {
            let entry = journal.entries[index].clone();
            let relative = checked_relative_path(&entry.relative_path)?;
            validate_existing_ancestors(&root, &relative)?;
            let target = root.join(relative);
            if snapshot(&target)? != entry.before { return Err(journal.mismatch(&entry.relative_path, "target changed after preparation")); }
            if entry.mutate {
                let saved = area.join(&entry.saved_relative_path);
                fs::rename(&target, &saved).map_err(|source| io_error(&target, source))?;
                sync_parent(&saved)?;
                sync_parent(&target)?;
                mutation += 1;
                maybe_crash(crash, mutation)?;
                maybe_request_cancel(cancellation, cancel, mutation);
                journal.entries[index].phase = Phase::Quarantined;
                self.persist_uninstall_journal(journal)?;
                if entry.desired != Snapshot::Missing {
                    let staged = area.join(&entry.staged_relative_path);
                    if snapshot(&staged)? != entry.desired { return Err(journal.mismatch(&entry.relative_path, "staged original changed")); }
                    if snapshot(&target)? != Snapshot::Missing { return Err(journal.mismatch(&entry.relative_path, "target appeared after quarantine")); }
                    fs::rename(&staged, &target).map_err(|source| io_error(&target, source))?;
                    sync_parent(&staged)?;
                    sync_parent(&target)?;
                    mutation += 1;
                    maybe_crash(crash, mutation)?;
                    maybe_request_cancel(cancellation, cancel, mutation);
                }
                journal.entries[index].phase = Phase::Restored;
                self.persist_uninstall_journal(journal)?;
            }
            if snapshot(&target)? != entry.desired { return Err(journal.mismatch(&entry.relative_path, "target outcome failed verification")); }
            journal.entries[index].phase = Phase::Verified;
            self.persist_uninstall_journal(journal)?;
        }
        self.verify_uninstall(journal)?;
        // A durable decision is written only after the entire chain has its exact expected outcome.
        journal.commit_decided = true;
        journal.cancel_requested_during_commit = cancellation.is_requested();
        self.persist_uninstall_journal(journal)?;
        maybe_crash(crash, mutation + 1)?;
        self.finish_uninstall(journal)
    }

    fn verify_uninstall(&self, journal: &UninstallJournal) -> Result<(), FileTransactionError> {
        let root = PathBuf::from(&journal.root_path);
        validate_uninstall_area(journal)?;
        for entry in &journal.entries {
            let relative = checked_relative_path(&entry.relative_path)?;
            validate_existing_ancestors(&root, &relative)?;
            if snapshot(&root.join(relative))? != entry.desired { return Err(journal.mismatch(&entry.relative_path, "committed target changed")); }
            if entry.mutate && snapshot(&journal.area().join(&entry.saved_relative_path))? != entry.before {
                return Err(journal.mismatch(&entry.relative_path, "quarantined installed bytes changed"));
            }
        }
        Ok(())
    }

    fn rollback_uninstall(&self, journal: &mut UninstallJournal) -> Result<(), FileTransactionError> {
        let root = PathBuf::from(&journal.root_path);
        let area = journal.area();
        validate_uninstall_area(journal)?;
        // Fence all unknown modifications before making even the first recovery write.
        for entry in &journal.entries { validate_rollback_entry(journal, entry)?; }
        for entry in journal.entries.iter().rev().filter(|entry| entry.mutate) {
            validate_rollback_entry(journal, entry)?;
            let target = root.join(checked_relative_path(&entry.relative_path)?);
            let saved = area.join(&entry.saved_relative_path);
            if snapshot(&saved)? == Snapshot::Missing { continue; }
            if snapshot(&target)? != Snapshot::Missing {
                fs::remove_file(&target).map_err(|source| io_error(&target, source))?;
                sync_parent(&target)?;
            }
            fs::rename(&saved, &target).map_err(|source| io_error(&saved, source))?;
            sync_parent(&saved)?;
            sync_parent(&target)?;
        }
        for entry in &journal.entries {
            if snapshot(&root.join(checked_relative_path(&entry.relative_path)?))? != entry.before {
                return Err(journal.mismatch(&entry.relative_path, "rollback verification failed"));
            }
        }
        journal.state = FileTransactionState::RolledBack;
        self.persist_uninstall_journal(journal)?;
        cleanup_dir(&area);
        Ok(())
    }

    fn insert_uninstall_journal(&self, journal: &UninstallJournal) -> Result<(), FileTransactionError> {
        write_uninstall_journal(journal)?;
        self.database.connection()?.execute(
            "INSERT INTO file_transactions(transaction_id,installation_id,owner_scope,root_identity,root_path,kind,state,journal_json,sequence,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,'uninstall',?6,?7,?8,?9,?9)",
            params![journal.transaction_id,journal.installation_id,journal.owner_scope,journal.root_identity,journal.root_path,journal.state.as_str(),serde_json::to_string(journal)?,journal.sequence,journal.created_at_ms],
        )?;
        Ok(())
    }

    fn persist_uninstall_journal(&self, journal: &mut UninstallJournal) -> Result<(), FileTransactionError> {
        journal.sequence = journal.sequence.checked_add(1).ok_or_else(|| FileTransactionError::InvalidRequest("journal sequence overflow".to_owned()))?;
        journal.updated_at_ms = now_ms()?;
        write_uninstall_journal(journal)?;
        let changed = self.database.connection()?.execute(
            "UPDATE file_transactions SET state=?2,journal_json=?3,sequence=?4,updated_at_ms=?5 WHERE transaction_id=?1",
            params![journal.transaction_id,journal.state.as_str(),serde_json::to_string(journal)?,journal.sequence,journal.updated_at_ms],
        )?;
        if changed != 1 { return Err(FileTransactionError::TransactionNotFound(journal.transaction_id.clone())); }
        Ok(())
    }

    fn finish_uninstall(&self, journal: &mut UninstallJournal) -> Result<(), FileTransactionError> {
        self.verify_uninstall(journal)?;
        let now = now_ms()?;
        let mut connection = self.database.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = if journal.entries.iter().any(|entry| !entry.mutate && entry.before != Snapshot::Missing) { "uninstalled-preserved" } else { "uninstalled" };
        for receipt_id in &journal.receipt_ids {
            let changed = transaction.execute("UPDATE install_receipts SET state=?2,updated_at_ms=?3 WHERE receipt_id=?1 AND state='active'", params![receipt_id,state,now])?;
            if changed != 1 { return Err(FileTransactionError::ReceiptInactive(receipt_id.clone())); }
        }
        journal.state = FileTransactionState::Committed;
        journal.updated_at_ms = now;
        transaction.execute("UPDATE file_transactions SET state='committed',journal_json=?2,updated_at_ms=?3 WHERE transaction_id=?1", params![journal.transaction_id,serde_json::to_string(journal)?,now])?;
        transaction.commit()?;
        Ok(())
    }

    fn uninstall_recovery_error(&self, journal: &mut UninstallJournal, reason: String) -> FileTransactionError {
        journal.state = FileTransactionState::RecoveryRequired;
        let persist = self.persist_uninstall_journal(journal).err();
        FileTransactionError::RecoveryRequired { transaction_id: journal.transaction_id.clone(), reason: match persist {
            Some(error) => format!("{reason}; journal update failed: {error}"), None => reason,
        } }
    }

    pub(super) fn recover_uninstall(&self, raw: &str, process_guard: &dyn TargetProcessGuard) -> Result<RecoveryReport, FileTransactionError> {
        let mut journal: UninstallJournal = serde_json::from_str(raw)?;
        let root = PathBuf::from(&journal.root_path);
        let result = (|| {
            if physical_root_identity(&root)? != journal.root_identity { return Err(journal.mismatch("root", "physical identity changed")); }
            let _lock = self.acquire_root_lock(&journal.root_identity)?;
            process_guard.ensure_stopped(&root).map_err(FileTransactionError::TargetRunning)?;
            let disk_path = journal.area().join("uninstall.json");
            validate_existing_ancestors(&root, Path::new(&format!("{TARGET_TRANSACTION_DIR}/{}/uninstall.json", journal.transaction_id)))?;
            let disk: UninstallJournal = serde_json::from_slice(&fs::read(&disk_path).map_err(|source| io_error(&disk_path, source))?)?;
            if disk.schema_version != 1 || disk.transaction_id != journal.transaction_id
                || disk.root_identity != journal.root_identity || disk.root_path != journal.root_path
                || disk.installation_id != journal.installation_id || disk.owner_scope != journal.owner_scope
                || disk.receipt_ids != journal.receipt_ids || disk.entries.len() != journal.entries.len()
                || disk.entries.iter().zip(&journal.entries).any(|(disk, database)| {
                    disk.relative_path != database.relative_path || disk.before != database.before
                        || disk.desired != database.desired || disk.mutate != database.mutate
                        || disk.staged_relative_path != database.staged_relative_path
                        || disk.saved_relative_path != database.saved_relative_path
                }) {
                return Err(journal.mismatch("journal", "file and database journal disagree"));
            }
            if disk.sequence >= journal.sequence { journal = disk; }
            if journal.commit_decided {
                self.finish_uninstall(&mut journal)?;
                Ok(RecoveryDisposition::RecoveredCommitted)
            } else {
                self.rollback_uninstall(&mut journal)?;
                Ok(RecoveryDisposition::RolledBack)
            }
        })();
        match result {
            Ok(disposition) => Ok(RecoveryReport { transaction_id: journal.transaction_id, disposition, detail: "uninstall chain recovered to its durable decision".to_owned(), receipt: None }),
            Err(FileTransactionError::RootBusy(owner)) => Ok(RecoveryReport { transaction_id: journal.transaction_id, disposition: RecoveryDisposition::RecoveryRequired, detail: format!("root remains busy: {owner}"), receipt: None }),
            Err(error) => {
                let detail = self.uninstall_recovery_error(&mut journal, error.to_string()).to_string();
                Ok(RecoveryReport { transaction_id: journal.transaction_id, disposition: RecoveryDisposition::RecoveryRequired, detail, receipt: None })
            }
        }
    }
}

fn validate_rollback_entry(journal: &UninstallJournal, entry: &UninstallEntry) -> Result<(), FileTransactionError> {
    validate_uninstall_area(journal)?;
    let root = PathBuf::from(&journal.root_path);
    let relative = checked_relative_path(&entry.relative_path)?;
    validate_existing_ancestors(&root, &relative)?;
    let current = snapshot(&root.join(relative))?;
    if !entry.mutate {
        if current != entry.before { return Err(journal.mismatch(&entry.relative_path, "preserved target changed during transaction")); }
        return Ok(());
    }
    let saved = snapshot(&journal.area().join(&entry.saved_relative_path))?;
    if saved == Snapshot::Missing {
        if current != entry.before { return Err(journal.mismatch(&entry.relative_path, "installed bytes missing without quarantine")); }
    } else if saved != entry.before || (current != Snapshot::Missing && current != entry.desired) {
        return Err(journal.mismatch(&entry.relative_path, "unknown external bytes fence rollback"));
    }
    Ok(())
}

fn validate_uninstall_area(journal: &UninstallJournal) -> Result<(), FileTransactionError> {
    let root = PathBuf::from(&journal.root_path);
    checked_relative_path(&journal.transaction_id)?;
    for child in ["staged/file", "saved/file", "uninstall.json"] {
        validate_existing_ancestors(&root, Path::new(&format!(
            "{TARGET_TRANSACTION_DIR}/{}/{child}", journal.transaction_id,
        )))?;
    }
    ensure_same_filesystem(&root, &journal.area())
}

fn write_uninstall_journal(journal: &UninstallJournal) -> Result<(), FileTransactionError> {
    validate_uninstall_area(journal)?;
    let area = journal.area();
    let temporary = area.join(format!("journal-{}.tmp", Uuid::new_v4()));
    write_create_new(&temporary, &serde_json::to_vec(journal)?)?;
    let destination = area.join("uninstall.json");
    fs::rename(&temporary, &destination).map_err(|source| io_error(&destination, source))?;
    sync_directory(&area)
}

fn snapshot(path: &Path) -> Result<Snapshot, FileTransactionError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Snapshot::Missing),
        Err(source) => return Err(io_error(path, source)),
    };
    if metadata.is_file() && !metadata.file_type().is_symlink() {
        return Ok(Snapshot::File { digest: hash_file(path)?, mode: mode(&metadata) });
    }
    let link = if metadata.file_type().is_symlink() {
        Some(fs::read_link(path).map_err(|source| io_error(path, source))?)
    } else { None };
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt as _;
        format!("{}:{}", metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let identity = format!("{:?}", metadata.created().ok());
    Ok(Snapshot::Foreign(format!("{identity}:{:?}:{link:?}:{:?}", metadata.file_type(), mode(&metadata))))
}
