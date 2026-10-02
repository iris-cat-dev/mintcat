use std::{fs::{self, File, OpenOptions}, path::PathBuf};
use fs2::FileExt;
use mintcat_runtime_kernel::RuntimeLayout;
use serde::{Deserialize, Serialize};
use crate::{atomic_write, BackupError};

pub struct MaintenanceAdmission { _lock: File }
pub struct MaintenanceGate { _lock: File, marker: PathBuf }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintenanceMarker {
    pub format_version: u32,
    pub owner_id: String,
    pub operation_id: Option<String>,
    pub phase: MaintenancePhase,
    pub snapshot_digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MaintenancePhase {
    ImportingEmptyOwner,
    RestoreCommitted,
    PreparingMigration,
    MigratedReadOnly,
    NewWrites,
}

impl MaintenanceGate {
    pub fn enter_mutation(layout: &RuntimeLayout) -> Result<MaintenanceAdmission, BackupError> {
        let file = lock_file(layout)?;
        FileExt::try_lock_shared(&file).map_err(|_| BackupError::Busy("maintenance lock held".into()))?;
        if marker_path(layout).try_exists()? { return Err(BackupError::RecoveryRequired); }
        Ok(MaintenanceAdmission { _lock: file })
    }
    /// `recovery` permits inspection of an unresolved marker, never implicit clearance.
    /// The caller must verify marker owner/operation identity before changing it.
    pub fn exclusive(layout: &RuntimeLayout, recovery: bool) -> Result<Self, BackupError> {
        let file = lock_file(layout)?;
        FileExt::try_lock_exclusive(&file).map_err(|_| BackupError::Busy("mutation admission held".into()))?;
        let marker = marker_path(layout);
        if !recovery && marker.try_exists()? { return Err(BackupError::RecoveryRequired); }
        Ok(Self { _lock: file, marker })
    }
    pub fn mark(&self, marker: &MaintenanceMarker) -> Result<(), BackupError> {
        atomic_write(&self.marker, &serde_json::to_vec(marker)?, true)
    }
    pub fn read(&self) -> Result<Option<MaintenanceMarker>, BackupError> {
        match fs::read(&self.marker) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub fn clear(&self) -> Result<(), BackupError> {
        fs::remove_file(&self.marker)?;
        crate::sync_directory(self.marker.parent().ok_or_else(|| BackupError::Invalid("marker parent absent".into()))?)?;
        Ok(())
    }
}
fn marker_path(layout: &RuntimeLayout) -> PathBuf {
    layout.state_dir.join("mintcat.feature-backup").join("maintenance.json")
}
fn lock_file(layout: &RuntimeLayout) -> Result<File, BackupError> {
    if !layout.initialized { return Err(BackupError::Invalid("runtime is not initialized".into())); }
    Ok(OpenOptions::new().create(true).truncate(false).read(true).write(true).open(layout.state_dir.join("maintenance.lock"))?)
}
