use std::{fs, path::Path};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use crate::{LegacyImportReport, LibraryError, LibrarySnapshot, OwnerStore};
use crate::store::read_snapshot;
use crate::legacy::OWNER_SCOPE;
const OUTCOME_SCOPE:&str="library.importLegacy";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct LegacyImportOutcome {pub snapshot_digest:String,pub result:LegacyImportReport,pub write_epoch:u64}
#[derive(Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Evidence {base_snapshot_digest:String,result:LegacyImportReport,write_epoch:u64}
pub(crate) fn epoch(path:&Path)->Result<u64,LibraryError> {
    let parent=path.parent().ok_or_else(||LibraryError::InvalidRecord("owner path missing".into()))?;
    match fs::read_to_string(parent.join("write-epoch")) {Ok(value)=>value.parse().map_err(|_|LibraryError::InvalidRecord("write fence is corrupt".into())),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(0),Err(error)=>Err(error.into())}
}
fn digest(snapshot:&LibrarySnapshot)->Result<String,LibraryError> {crate::digest::serialized_digest(snapshot)}
pub(crate) fn advance(path:&Path)->Result<(),LibraryError> {
    use std::io::Write;
    let value=epoch(path)?.checked_add(1).ok_or_else(||LibraryError::InvalidRecord("write fence exhausted".into()))?;
    let parent=path.parent().ok_or_else(||LibraryError::InvalidRecord("owner path missing".into()))?;
    let temporary=parent.join(format!(".write-epoch-{}",uuid::Uuid::new_v4()));
    let mut options=fs::OpenOptions::new();options.write(true).create_new(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    let mut file=options.open(&temporary)?;write!(file,"{value}")?;file.sync_all()?;drop(file);
    fs::rename(temporary,parent.join("write-epoch"))?;fs::File::open(parent)?.sync_all()?;Ok(())
}
/// The evidence row is excluded from its base hash to avoid a self-referential digest.
/// Lookup verifies this exact base, then hashes the complete snapshot including the row.
pub(crate) fn record(transaction:&Transaction<'_>,path:&Path,result:&LegacyImportReport)->Result<(),LibraryError> {
    let base=read_snapshot(transaction)?;
    let evidence=Evidence {base_snapshot_digest:digest(&base)?,result:result.clone(),write_epoch:epoch(path)?};
    transaction.execute("INSERT INTO library_mutation_results(owner_scope,idempotency_scope,idempotency_key,inputs_hash,result_json,operation_id,created_at_ms) VALUES(?1,?2,?3,?3,?4,?5,?6)",rusqlite::params![OWNER_SCOPE,OUTCOME_SCOPE,result.source_digest,serde_json::to_string(&evidence)?,uuid::Uuid::new_v4().to_string(),base.import_runs.iter().find(|r|r.source_digest==result.source_digest).ok_or(LibraryError::StaleOperation)?.imported_at_ms])?;
    Ok(())
}
pub(crate) fn lookup(path:&Path,source_digest:&str)->Result<Option<LegacyImportOutcome>,LibraryError> {
    let mut connection=Connection::open_with_flags(path,OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    let transaction=connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let value:Option<String>=transaction.query_row("SELECT result_json FROM library_mutation_results WHERE owner_scope=?1 AND idempotency_scope=?2 AND idempotency_key=?3",[OWNER_SCOPE,OUTCOME_SCOPE,source_digest],|row|row.get(0)).optional()?;
    let Some(value)=value else {return Ok(None)};
    let evidence:Evidence=serde_json::from_str(&value)?;
    if epoch(path)?!=evidence.write_epoch {return Err(LibraryError::StaleOperation);}
    let snapshot=read_snapshot(&transaction)?;let snapshot_digest=digest(&snapshot)?;
    let mut base=snapshot;
    base.mutation_results.retain(|r|!(r.owner_scope==OWNER_SCOPE && r.idempotency_scope==OUTCOME_SCOPE && r.idempotency_key==source_digest));
    if digest(&base)?!=evidence.base_snapshot_digest {return Err(LibraryError::StaleOperation);}
    transaction.commit()?;
    Ok(Some(LegacyImportOutcome {snapshot_digest,result:evidence.result,write_epoch:evidence.write_epoch}))
}
impl OwnerStore {
    pub fn legacy_import_outcome(&self,source_digest:&str)->Result<Option<LegacyImportOutcome>,LibraryError> {lookup(self.database_path(),source_digest)}
}
