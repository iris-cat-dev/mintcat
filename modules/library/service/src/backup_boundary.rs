use rusqlite::{Transaction, TransactionBehavior};

use crate::{store::{insert_snapshot, read_snapshot, validate_snapshot}, LibraryError, LibrarySnapshot, OwnerStore};

impl OwnerStore { /// Caller must hold the runtime exclusive maintenance admission gate.
/// This owner transaction also detects work admitted before that gate existed.
pub fn backup_snapshot(&self) -> Result<LibrarySnapshot, LibraryError> {
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_quiescent(&transaction)?;
    let snapshot = read_snapshot(&transaction)?;
    transaction.commit()?;
    Ok(snapshot)
}

pub fn ensure_backup_quiescent(&self) -> Result<(), LibraryError> {
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_quiescent(&transaction)?;
    transaction.commit()?;
    Ok(())
}

/// Logical import into an empty owner only; never replace or roll back live state.
/// Every insert and all FK/constraint checks share one SQLite transaction.
pub fn import_empty_snapshot(&self, snapshot: &LibrarySnapshot) -> Result<(), LibraryError> {
    validate_snapshot(snapshot)?;
    if let Some(operation) = snapshot.download_operations.iter().find(|operation| {
        !["succeeded", "cancelled", "failed"].contains(&operation.state.as_str())
    }) {
        return Err(LibraryError::ResourceBusy { owner_operation_id: operation.operation_id.clone() });
    }
    if let Some(lease) = snapshot.operation_leases.iter().find(|lease| {
        ["active", "suspect"].contains(&lease.state.as_str())
    }) {
        return Err(LibraryError::ResourceBusy { owner_operation_id: lease.operation_id.clone() });
    }
    let mut connection = self.connection()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_quiescent(&transaction)?;
    if read_snapshot(&transaction)? != LibrarySnapshot::default() {
        return Err(LibraryError::LibraryNotEmpty);
    }
    let guards: u64 = transaction.query_row("SELECT COUNT(*) FROM library_profile_content_guards", [], |row| row.get(0))?;
    if guards != 0 {
        return Err(LibraryError::LibraryNotEmpty);
    }
    insert_snapshot(&transaction, snapshot)?;
    transaction.commit()?;
    Ok(())
} }

pub(crate) fn ensure_quiescent(transaction: &Transaction<'_>) -> Result<(), LibraryError> {
    let busy: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_download_operations WHERE state IN ('queued', 'running', 'recovery-required'))
          OR EXISTS(SELECT 1 FROM library_operation_leases WHERE state IN ('active', 'suspect'))
          OR EXISTS(SELECT 1 FROM library_profile_content_guards WHERE state = 'active')",
        [], |row| row.get(0),
    )?;
    if busy {
        return Err(LibraryError::ResourceBusy { owner_operation_id: "backup-maintenance".to_owned() });
    }
    Ok(())
}
