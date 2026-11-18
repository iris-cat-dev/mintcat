use std::{collections::BTreeSet, fs::File, path::Path, sync::{Arc, Mutex}, time::Duration};
use mintcat_runtime_artifacts::ArtifactGrant;
use mintcat_runtime_kernel::RuntimeLayout;
use mintcat_sdk::*;
use serde_json::Value;
use crate::*;

/// Daemon-owned single writer over the existing Library database and recovery fence.
#[derive(Clone, Debug)]
pub struct LibraryStore { owner: OwnerStore, writer: Arc<Mutex<()>> }
impl LibraryStore {
    pub fn open(layout: &RuntimeLayout) -> Result<Self, LibraryError> {
        Ok(Self { owner: OwnerStore::open(layout)?, writer: Arc::new(Mutex::new(())) })
    }
    pub fn open_for_recovery(layout: &RuntimeLayout) -> Result<Self, LibraryError> {
        let path = layout.state_dir.join("mintcat.library/library.sqlite");
        if !path.exists() { return Self::open(layout); }
        Ok(Self { owner: OwnerStore::open_read_only(&path)?, writer: Arc::new(Mutex::new(())) })
    }
    pub fn resume_after_recovery(&self) -> Result<(), LibraryError> { self.owner.resume_after_recovery() }
    pub fn database_path(&self) -> &Path { self.owner.database_path() }
    pub fn write_epoch(&self) -> Result<u64, LibraryError> { crate::import_outcome::epoch(self.database_path()) }
    fn advance_epoch(&self) -> Result<(), LibraryError> {
        self.owner.ensure_writable()?;
        crate::import_outcome::advance(self.database_path())
    }
    pub fn import_legacy(&self, source: &Path, vault: &dyn CredentialVault) -> Result<crate::LegacyImportReport, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.owner.ensure_writable()?;
        self.owner.import_legacy(source, vault)
    }
    pub fn import_legacy_checked(&self, source: &Path, expected_source_digest: &str, vault: &dyn CredentialVault) -> Result<crate::LegacyImportReport, LibraryError> {
        let _writer = self.writer.lock().map_err(|_| LibraryError::Poisoned)?;
        self.owner.ensure_writable()?;
        self.owner.import_legacy_checked(source, expected_source_digest, vault)
    }
    /// Freeze an approved legacy source, including committed WAL, without writing the source.
    pub fn snapshot_legacy_source(source: &Path, target: &Path) -> Result<(), LibraryError> {
        if target.exists() { return Err(LibraryError::BackupExists(target.into())); }
        let input = rusqlite::Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        let reserve = std::fs::OpenOptions::new().write(true).create_new(true).open(target)?;
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; reserve.set_permissions(std::fs::Permissions::from_mode(0o600))?; }
        drop(reserve);
        let mut output = rusqlite::Connection::open(target)?;
        let backup = rusqlite::backup::Backup::new(&input, &mut output)?;
        backup.run_to_completion(128, Duration::from_millis(10), None)?;
        drop(backup); drop(output); File::open(target)?.sync_all()?;
        Ok(())
    }
    pub fn legacy_source_digest(source: &Path) -> Result<String, LibraryError> {
        crate::legacy::legacy_source_digest(source)
    }
    pub fn snapshot_digest(snapshot: &LibrarySnapshot) -> Result<String, LibraryError> {
        crate::digest::serialized_digest(snapshot)
    }
    pub fn snapshot_file_digest(source: &Path) -> Result<String, LibraryError> {
        let mut writer=crate::digest::DigestWriter::new(std::io::sink());
        std::io::copy(&mut File::open(source)?, &mut writer)?;
        Ok(writer.finish().1)
    }
    pub fn export_to(&self,target:&Path)->Result<LibraryBackup,LibraryError> {
        if target.exists() { return Err(LibraryError::BackupExists(target.into())); }
        let backup=LibraryBackup { format:LIBRARY_BACKUP_FORMAT.into(),exported_at_ms:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64,snapshot:self.snapshot()? };
        let parent=target.parent().ok_or_else(|| LibraryError::InvalidRecord("backup path has no parent".into()))?;
        std::fs::create_dir_all(parent)?;
        let temporary=parent.join(format!(".library-backup-{}",uuid::Uuid::new_v4()));
        let result=(|| -> Result<(),LibraryError> {
            let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
            #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
            let mut file=options.open(&temporary)?;
            serde_json::to_writer(&mut file,&backup)?;file.sync_all()?;drop(file);
            match std::fs::hard_link(&temporary,target) {
                Ok(())=>{},
                Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>return Err(LibraryError::BackupExists(target.into())),
                Err(error)=>return Err(error.into()),
            }
            File::open(parent)?.sync_all()?;Ok(())
        })();
        let _=std::fs::remove_file(&temporary);result?;Ok(backup)
    }
    pub fn restore_from(&self,source:&Path)->Result<LibrarySnapshot,LibraryError> {
        let backup:LibraryBackup=serde_json::from_reader(File::open(source)?)?;
        if backup.format!=LIBRARY_BACKUP_FORMAT { return Err(LibraryError::UnsupportedBackup(backup.format)); }
        self.import_empty_snapshot(&backup.snapshot)?;Ok(backup.snapshot)
    }
}
include!("client_methods.rs");
