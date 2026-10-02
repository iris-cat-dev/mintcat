mod metadata;
mod package;
mod repository;
mod scope;

pub use metadata::*;
pub use package::*;
pub use repository::*;
pub use scope::*;

use std::{fs::{self, OpenOptions}, io::Write, path::Path};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReleaseError {
    #[error("release I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("release JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("artifact validation: {0}")]
    Artifact(#[from] mintcat_runtime_artifacts::ArtifactError),
    #[error("release rejected: {0}")]
    Rejected(String),
}
pub type Result<T, E = ReleaseError> = std::result::Result<T, E>;

pub(crate) fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok { Ok(()) } else { Err(ReleaseError::Rejected(message.into())) }
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let metadata = fs::symlink_metadata(path)?;
    require(metadata.is_file() && !metadata.file_type().is_symlink(), "metadata must be a regular file")?;
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
pub(crate) fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    // Value maps are sorted by serde_json's default BTreeMap implementation.
    Ok(serde_json::to_vec(&serde_json::to_value(value)?)?)
}
pub(crate) fn parent_directory(path: &Path) -> &Path {
    path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

pub(crate) fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = parent_directory(path);
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".release-{}", uuid::Uuid::new_v4()));
    let result: Result<()> = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&canonical(value)?)?;
        file.sync_all()?;
        drop(file);
        commit_rename(&tmp, path)?;
        Ok(())
    })();
    if tmp.exists() { let _ = fs::remove_file(&tmp); }
    result
}

/// Commit a same-filesystem rename with the platform's durable publication primitive.
pub(crate) fn commit_rename(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        let mut source = source.as_os_str().encode_wide().collect::<Vec<_>>();
        let mut destination = destination.as_os_str().encode_wide().collect::<Vec<_>>();
        require(!source.contains(&0) && !destination.contains(&0), "path contains a NUL")?;
        source.push(0);
        destination.push(0);
        const REPLACE_EXISTING: u32 = 0x1;
        const WRITE_THROUGH: u32 = 0x8;
        // Both pointers are valid NUL-terminated UTF-16 buffers for this call.
        if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), REPLACE_EXISTING | WRITE_THROUGH) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(windows))] {
        fs::rename(source, destination)?;
        std::fs::File::open(parent_directory(destination))?.sync_all()?;
        Ok(())
    }
}
