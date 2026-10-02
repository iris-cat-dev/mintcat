use std::{
    collections::BTreeMap,
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path},
};

use thiserror::Error;

/// Media type for a deterministic, immutable local-directory snapshot artifact.
pub const DIRECTORY_SNAPSHOT_MEDIA_TYPE: &str = "application/vnd.mintcat.directory-snapshot";

/// Versioned wire prefix for a directory snapshot artifact.
///
/// The payload is: magic, big-endian entry count, then repeated
/// `(u32 path length, UTF-8 relative path, u64 content length, content bytes)` records.
pub const DIRECTORY_SNAPSHOT_MAGIC: &[u8] = b"MINTCAT-DIRECTORY-SNAPSHOT\0\x01";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectorySnapshotLimits {
    pub max_entries: u64,
    pub max_path_bytes: usize,
    pub max_entry_bytes: u64,
    pub max_total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySnapshotEntry {
    pub relative_path: String,
    pub content_offset: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySnapshotIndex {
    pub entries: Vec<DirectorySnapshotEntry>,
    pub total_bytes: u64,
}

#[derive(Debug, Error)]
pub enum DirectorySnapshotError {
    #[error("directory snapshot I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid directory snapshot: {0}")]
    Invalid(String),
    #[error("directory snapshot limit exceeded: {0}")]
    Limit(String),
}

pub fn read_directory_snapshot_index(
    reader: &mut (impl Read + Seek),
    limits: DirectorySnapshotLimits,
) -> Result<DirectorySnapshotIndex, DirectorySnapshotError> {
    if limits.max_entries == 0
        || limits.max_path_bytes == 0
        || limits.max_entry_bytes == 0
        || limits.max_total_bytes == 0
    {
        return Err(DirectorySnapshotError::Limit(
            "all parser limits must be positive".to_owned(),
        ));
    }
    let file_length = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut magic = vec![0_u8; DIRECTORY_SNAPSHOT_MAGIC.len()];
    read_exact(reader, &mut magic)?;
    if magic != DIRECTORY_SNAPSHOT_MAGIC {
        return Err(DirectorySnapshotError::Invalid(
            "magic/version differs".to_owned(),
        ));
    }
    let count = read_u64(reader)?;
    if count > limits.max_entries {
        return Err(DirectorySnapshotError::Limit(format!(
            "entry count exceeds {}",
            limits.max_entries
        )));
    }
    let capacity = usize::try_from(count).map_err(|_| {
        DirectorySnapshotError::Limit("entry count does not fit this platform".to_owned())
    })?;
    let mut entries = Vec::with_capacity(capacity);
    let mut paths = BTreeMap::new();
    let mut total_bytes = 0_u64;
    for _ in 0..count {
        let path_length = read_u32(reader)? as usize;
        if path_length == 0 || path_length > limits.max_path_bytes {
            return Err(DirectorySnapshotError::Limit(format!(
                "path exceeds {} bytes",
                limits.max_path_bytes
            )));
        }
        let mut path_bytes = vec![0_u8; path_length];
        read_exact(reader, &mut path_bytes)?;
        let path = std::str::from_utf8(&path_bytes)
            .map_err(|_| DirectorySnapshotError::Invalid("path is not UTF-8".to_owned()))?;
        let path = checked_snapshot_path(path)?;
        register_path(&mut paths, &path)?;
        let size = read_u64(reader)?;
        if size > limits.max_entry_bytes {
            return Err(DirectorySnapshotError::Limit(format!(
                "entry exceeds {} bytes",
                limits.max_entry_bytes
            )));
        }
        total_bytes = total_bytes
            .checked_add(size)
            .ok_or_else(|| DirectorySnapshotError::Limit("total byte count overflow".to_owned()))?;
        if total_bytes > limits.max_total_bytes {
            return Err(DirectorySnapshotError::Limit(format!(
                "total bytes exceed {}",
                limits.max_total_bytes
            )));
        }
        let content_offset = reader.stream_position()?;
        let end = content_offset
            .checked_add(size)
            .ok_or_else(|| DirectorySnapshotError::Invalid("entry offset overflow".to_owned()))?;
        if end > file_length {
            return Err(DirectorySnapshotError::Invalid(
                "entry exceeds artifact length".to_owned(),
            ));
        }
        entries.push(DirectorySnapshotEntry {
            relative_path: path,
            content_offset,
            size,
        });
        reader.seek(SeekFrom::Start(end))?;
    }
    if reader.stream_position()? != file_length {
        return Err(DirectorySnapshotError::Invalid(
            "trailing bytes after final entry".to_owned(),
        ));
    }
    Ok(DirectorySnapshotIndex {
        entries,
        total_bytes,
    })
}

fn read_exact(reader: &mut impl Read, bytes: &mut [u8]) -> Result<(), DirectorySnapshotError> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            DirectorySnapshotError::Invalid("payload ended early".to_owned())
        } else {
            DirectorySnapshotError::Io(error)
        }
    })
}

fn read_u32(reader: &mut impl Read) -> Result<u32, DirectorySnapshotError> {
    let mut bytes = [0_u8; 4];
    read_exact(reader, &mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> Result<u64, DirectorySnapshotError> {
    let mut bytes = [0_u8; 8];
    read_exact(reader, &mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

fn checked_snapshot_path(value: &str) -> Result<String, DirectorySnapshotError> {
    if value.is_empty()
        || value.starts_with(['/', '\\'])
        || value.contains('\\')
        || value.contains(':')
        || value.contains('\0')
    {
        return Err(DirectorySnapshotError::Invalid(
            "entry path is not portable and relative".to_owned(),
        ));
    }
    let mut normalized = String::new();
    for component in Path::new(value).components() {
        let Component::Normal(component) = component else {
            return Err(DirectorySnapshotError::Invalid(
                "entry path contains traversal".to_owned(),
            ));
        };
        let component = component
            .to_str()
            .ok_or_else(|| DirectorySnapshotError::Invalid("path is not UTF-8".to_owned()))?;
        if component.is_empty() || component == "." || component == ".." {
            return Err(DirectorySnapshotError::Invalid(
                "entry path contains traversal".to_owned(),
            ));
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component);
    }
    Ok(normalized)
}

fn register_path(
    paths: &mut BTreeMap<String, bool>,
    path: &str,
) -> Result<(), DirectorySnapshotError> {
    let key = path.to_ascii_lowercase();
    if paths.contains_key(&key) {
        return Err(DirectorySnapshotError::Invalid(format!(
            "duplicate or case-colliding path {path}"
        )));
    }
    let mut prefix = String::new();
    let components = key.split('/').collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        if paths.get(&prefix) == Some(&false) {
            return Err(DirectorySnapshotError::Invalid(format!(
                "file {prefix} collides with child path {path}"
            )));
        }
    }
    let child_prefix = format!("{key}/");
    if paths
        .keys()
        .any(|existing| existing.starts_with(&child_prefix))
    {
        return Err(DirectorySnapshotError::Invalid(format!(
            "file {path} collides with an existing child path"
        )));
    }
    paths.insert(key, false);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn indexes_valid_payload_and_rejects_trailing_or_colliding_paths() {
        let payload = |entries: &[(&str, &[u8])]| {
            let mut bytes = DIRECTORY_SNAPSHOT_MAGIC.to_vec();
            bytes.extend_from_slice(&(entries.len() as u64).to_be_bytes());
            for (path, content) in entries {
                bytes.extend_from_slice(&(path.len() as u32).to_be_bytes());
                bytes.extend_from_slice(path.as_bytes());
                bytes.extend_from_slice(&(content.len() as u64).to_be_bytes());
                bytes.extend_from_slice(content);
            }
            bytes
        };
        let limits = DirectorySnapshotLimits {
            max_entries: 8,
            max_path_bytes: 128,
            max_entry_bytes: 1024,
            max_total_bytes: 4096,
        };
        let index = read_directory_snapshot_index(
            &mut Cursor::new(payload(&[("Content/a", b"a"), ("js/main.js", b"js")])),
            limits,
        )
        .unwrap();
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.total_bytes, 3);

        let mut trailing = payload(&[("Content/a", b"a")]);
        trailing.push(0);
        assert!(matches!(
            read_directory_snapshot_index(&mut Cursor::new(trailing), limits),
            Err(DirectorySnapshotError::Invalid(_))
        ));
        assert!(matches!(
            read_directory_snapshot_index(
                &mut Cursor::new(payload(&[("Content/A", b"a"), ("content/a", b"b")])),
                limits
            ),
            Err(DirectorySnapshotError::Invalid(_))
        ));
    }
}
