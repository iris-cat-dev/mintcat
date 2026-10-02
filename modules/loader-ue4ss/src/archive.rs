use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path},
};

use mintcat_unreal_pak::{normalize_entry_path, PakArchive};
use zip::ZipArchive;

use crate::{LoaderError, LoaderLimits};

#[derive(Debug, Clone)]
pub(crate) struct ZipEntry {
    pub index: usize,
    pub path: String,
    pub size: u64,
    pub directory: bool,
}

pub(crate) fn scan_zip(
    archive: &mut ZipArchive<File>,
    input_id: &str,
    limits: LoaderLimits,
) -> Result<Vec<ZipEntry>, LoaderError> {
    if archive.len() as u64 > limits.max_entries {
        return Err(LoaderError::LimitExceeded {
            input_id: input_id.to_owned(),
            limit: format!("ZIP entry count exceeds {}", limits.max_entries),
        });
    }
    let mut result = Vec::with_capacity(archive.len());
    let mut paths = BTreeMap::new();
    let mut expanded = 0_u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| LoaderError::Archive {
                input_id: input_id.to_owned(),
                reason: error.to_string(),
            })?;
        validate_zip_type(&entry).map_err(|reason| LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason,
        })?;
        let raw_name =
            std::str::from_utf8(entry.name_raw()).map_err(|_| LoaderError::InvalidInput {
                input_id: input_id.to_owned(),
                reason: "ZIP entry name is not UTF-8".to_owned(),
            })?;
        let directory = entry.is_dir();
        let raw_name = raw_name.trim_end_matches('/');
        let path = checked_relative_path(raw_name, limits.max_path_bytes).map_err(|reason| {
            LoaderError::InvalidInput {
                input_id: input_id.to_owned(),
                reason: format!("invalid ZIP entry path: {reason}"),
            }
        })?;
        register_path(&mut paths, &path, directory).map_err(|reason| {
            LoaderError::InvalidInput {
                input_id: input_id.to_owned(),
                reason: format!("invalid ZIP layout: {reason}"),
            }
        })?;
        if !directory {
            let size = entry.size();
            if size > limits.max_entry_bytes {
                return Err(LoaderError::LimitExceeded {
                    input_id: input_id.to_owned(),
                    limit: format!("ZIP entry exceeds {} bytes", limits.max_entry_bytes),
                });
            }
            if size > 0
                && (entry.compressed_size() == 0
                    || size
                        > entry
                            .compressed_size()
                            .saturating_mul(limits.max_compression_ratio))
            {
                return Err(LoaderError::LimitExceeded {
                    input_id: input_id.to_owned(),
                    limit: format!(
                        "ZIP compression ratio exceeds {}",
                        limits.max_compression_ratio
                    ),
                });
            }
            expanded = expanded
                .checked_add(size)
                .ok_or_else(|| LoaderError::LimitExceeded {
                    input_id: input_id.to_owned(),
                    limit: "ZIP expanded byte count overflow".to_owned(),
                })?;
            if expanded > limits.max_expanded_bytes {
                return Err(LoaderError::LimitExceeded {
                    input_id: input_id.to_owned(),
                    limit: format!("ZIP expanded bytes exceed {}", limits.max_expanded_bytes),
                });
            }
        }
        result.push(ZipEntry {
            index,
            path,
            size: entry.size(),
            directory,
        });
    }
    Ok(result)
}

pub(crate) fn checked_relative_path(value: &str, max_bytes: usize) -> Result<String, String> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(format!(
            "path must contain between 1 and {max_bytes} UTF-8 bytes"
        ));
    }
    if value.starts_with(['/', '\\'])
        || value.contains('\\')
        || value.contains(':')
        || value.contains('\0')
    {
        return Err("path must use portable forward-slash relative form".to_owned());
    }
    let mut normalized = String::new();
    for component in Path::new(value).components() {
        let Component::Normal(component) = component else {
            return Err("absolute and traversal components are forbidden".to_owned());
        };
        let component = component
            .to_str()
            .ok_or_else(|| "path must be UTF-8".to_owned())?;
        validate_component(component)?;
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component);
    }
    if normalized.is_empty() {
        return Err("path must contain a file component".to_owned());
    }
    Ok(normalized)
}

pub(crate) fn checked_component(value: &str) -> Result<String, String> {
    validate_component(value)?;
    if value.contains('/') || value.contains('\\') || value.contains(':') {
        return Err("identifier must be one portable path component".to_owned());
    }
    Ok(value.to_owned())
}

fn validate_component(component: &str) -> Result<(), String> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.ends_with(['.', ' '])
        || component.chars().any(|character| {
            character.is_control() || matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
        })
    {
        return Err(format!("invalid portable path component {component:?}"));
    }
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || upper.strip_prefix("COM").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || upper.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        });
    if reserved {
        return Err(format!("reserved Windows path component {component:?}"));
    }
    Ok(())
}

fn register_path(
    paths: &mut BTreeMap<String, bool>,
    path: &str,
    directory: bool,
) -> Result<(), String> {
    let key = path.to_lowercase();
    if let Some(existing_directory) = paths.get(&key) {
        if *existing_directory && directory {
            return Ok(());
        }
        return Err(format!("duplicate or case-colliding path {path}"));
    }
    let components = key.split('/').collect::<Vec<_>>();
    let mut prefix = String::new();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        if paths.get(&prefix) == Some(&false) {
            return Err(format!("file {prefix} collides with child path {path}"));
        }
    }
    if !directory {
        let child_prefix = format!("{key}/");
        if paths
            .keys()
            .any(|existing| existing.starts_with(&child_prefix))
        {
            return Err(format!("file {path} collides with an existing child path"));
        }
    }
    paths.insert(key, directory);
    Ok(())
}

fn validate_zip_type(entry: &zip::read::ZipFile<'_>) -> Result<(), String> {
    let Some(mode) = entry.unix_mode() else {
        return Ok(());
    };
    let file_type = mode & 0o170000;
    if file_type == 0 || file_type == 0o100000 || (file_type == 0o040000 && entry.is_dir()) {
        Ok(())
    } else {
        Err("ZIP contains a link or special file".to_owned())
    }
}

pub(crate) fn validate_pe_dll(
    file: &mut File,
    input_id: &str,
    max_bytes: u64,
) -> Result<(), LoaderError> {
    let length = file
        .metadata()
        .map_err(|source| LoaderError::Io {
            scope: "reading native module metadata".to_owned(),
            source,
        })?
        .len();
    if length == 0 || length > max_bytes {
        return Err(LoaderError::LimitExceeded {
            input_id: input_id.to_owned(),
            limit: format!("native DLL must contain between 1 and {max_bytes} bytes"),
        });
    }
    let mut dos = [0_u8; 64];
    file.read_exact(&mut dos)
        .map_err(|_| LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: "native module is not a complete PE file".to_owned(),
        })?;
    if &dos[..2] != b"MZ" {
        return Err(LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: "native module lacks the DOS MZ signature".to_owned(),
        });
    }
    let pe_offset = u32::from_le_bytes(dos[0x3c..0x40].try_into().expect("fixed slice")) as u64;
    if pe_offset > length.saturating_sub(24) {
        return Err(LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: "native module has an invalid PE header offset".to_owned(),
        });
    }
    file.seek(SeekFrom::Start(pe_offset))
        .map_err(|source| LoaderError::Io {
            scope: "seeking native PE header".to_owned(),
            source,
        })?;
    let mut header = [0_u8; 24];
    file.read_exact(&mut header)
        .map_err(|_| LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: "native module PE header ended early".to_owned(),
        })?;
    if &header[..4] != b"PE\0\0" {
        return Err(LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: "native module lacks the PE signature".to_owned(),
        });
    }
    let characteristics = u16::from_le_bytes([header[22], header[23]]);
    if characteristics & 0x2000 == 0 {
        return Err(LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: "native PE image is not marked as a DLL".to_owned(),
        });
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|source| LoaderError::Io {
            scope: "rewinding native module".to_owned(),
            source,
        })?;
    Ok(())
}

pub(crate) fn validate_audio_pak(
    file: &mut File,
    input_id: &str,
    limits: LoaderLimits,
) -> Result<(), LoaderError> {
    let archive = PakArchive::open(file)?;
    if archive.metadata().encrypted_index {
        return Err(LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: "audio PAK indexes must not be encrypted".to_owned(),
        });
    }
    if archive.metadata().files.is_empty() {
        return Err(LoaderError::InvalidInput {
            input_id: input_id.to_owned(),
            reason: "audio PAK contains no files".to_owned(),
        });
    }
    if archive.metadata().files.len() as u64 > limits.max_entries {
        return Err(LoaderError::LimitExceeded {
            input_id: input_id.to_owned(),
            limit: format!("PAK entry count exceeds {}", limits.max_entries),
        });
    }
    for stored_path in &archive.metadata().files {
        let path = normalize_entry_path(&archive.metadata().mount_point, stored_path)?;
        checked_relative_path(&path, limits.max_path_bytes).map_err(|reason| {
            LoaderError::InvalidInput {
                input_id: input_id.to_owned(),
                reason: format!("invalid PAK entry path: {reason}"),
            }
        })?;
        let lowercase = path.to_ascii_lowercase();
        if lowercase.contains("initspacerig")
            || lowercase.contains("initcave")
            || lowercase.ends_with("assetregistry.bin")
            || lowercase.ends_with(".ushaderbytecode")
        {
            return Err(LoaderError::InvalidInput {
                input_id: input_id.to_owned(),
                reason: format!("PAK contains game-state content {path}"),
            });
        }
    }
    Ok(())
}
