use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path},
};

use mintcat_sdk::{read_directory_snapshot_index, DirectorySnapshotError, DirectorySnapshotLimits};
use mintcat_unreal_pak::{normalize_entry_path, PakArchive};
use zip::ZipArchive;

use crate::{
    io_error, InputInspection, PakContainerInspection, UnrealEngineError, UnrealEngineLimits,
    UnrealInput, UnrealInputFormat,
};

pub(crate) struct Candidate {
    pub input_id: String,
    pub relative_path: String,
    pub key: String,
    pub payload: CandidatePayload,
}

#[derive(Clone)]
pub(crate) enum CandidatePayload {
    Pak {
        source_index: usize,
        stored_path: String,
    },
    Snapshot {
        source_index: usize,
        offset: u64,
        size: u64,
    },
}

struct PakSource {
    input_id: String,
    file: File,
    archive: PakArchive,
}

struct SnapshotSource {
    input_id: String,
    file: File,
}

pub(crate) struct MaterializedInputs {
    pub inspections: Vec<InputInspection>,
    pub candidates: Vec<Candidate>,
    pak_sources: Vec<PakSource>,
    snapshot_sources: Vec<SnapshotSource>,
    limits: UnrealEngineLimits,
    observed_entries: u64,
}

impl MaterializedInputs {
    pub fn open(
        inputs: Vec<UnrealInput<'_>>,
        limits: UnrealEngineLimits,
        workspace: &Path,
    ) -> Result<Self, UnrealEngineError> {
        let mut ids = BTreeSet::new();
        for input in &inputs {
            if input.input_id.trim().is_empty() {
                return Err(UnrealEngineError::InvalidRequest(
                    "every Unreal input requires an id".to_owned(),
                ));
            }
            if !ids.insert(input.input_id.clone()) {
                return Err(UnrealEngineError::DuplicateInput(input.input_id.clone()));
            }
            if input.artifact.descriptor.size > limits.max_input_bytes {
                return Err(UnrealEngineError::LimitExceeded {
                    input_id: input.input_id.clone(),
                    limit: format!("input exceeds {} bytes", limits.max_input_bytes),
                });
            }
        }

        let mut materialized = Self {
            inspections: Vec::with_capacity(inputs.len()),
            candidates: Vec::new(),
            pak_sources: Vec::new(),
            snapshot_sources: Vec::new(),
            limits,
            observed_entries: 0,
        };
        for (index, input) in inputs.into_iter().enumerate() {
            materialized.open_input(index, input, workspace)?;
        }
        Ok(materialized)
    }

    pub fn copy_candidate(
        &mut self,
        candidate_index: usize,
        output: &mut impl Write,
    ) -> Result<(), UnrealEngineError> {
        let candidate = self
            .candidates
            .get(candidate_index)
            .ok_or_else(|| UnrealEngineError::InvalidRequest("candidate index".to_owned()))?;
        match candidate.payload.clone() {
            CandidatePayload::Pak {
                source_index,
                stored_path,
            } => {
                let source = self.pak_sources.get_mut(source_index).ok_or_else(|| {
                    UnrealEngineError::InvalidRequest("PAK source index".to_owned())
                })?;
                source
                    .archive
                    .read_file_to(&stored_path, &mut source.file, output)
                    .map_err(|error| UnrealEngineError::Archive {
                        input_id: source.input_id.clone(),
                        reason: error.to_string(),
                    })
            }
            CandidatePayload::Snapshot {
                source_index,
                offset,
                size,
            } => {
                let source = self.snapshot_sources.get_mut(source_index).ok_or_else(|| {
                    UnrealEngineError::InvalidRequest("snapshot source index".to_owned())
                })?;
                source
                    .file
                    .seek(SeekFrom::Start(offset))
                    .map_err(|source_error| {
                        io_error(
                            format!("seeking directory input {}", source.input_id),
                            source_error,
                        )
                    })?;
                let copied = io::copy(&mut Read::by_ref(&mut source.file).take(size), output)
                    .map_err(|source_error| {
                        io_error(
                            format!("reading directory input {}", source.input_id),
                            source_error,
                        )
                    })?;
                if copied != size {
                    return Err(UnrealEngineError::Archive {
                        input_id: source.input_id.clone(),
                        reason: "directory entry ended before its declared size".to_owned(),
                    });
                }
                Ok(())
            }
        }
    }

    fn open_input(
        &mut self,
        index: usize,
        input: UnrealInput<'_>,
        workspace: &Path,
    ) -> Result<(), UnrealEngineError> {
        let mut inspection = InputInspection {
            input_id: input.input_id.clone(),
            format: input.format,
            files: Vec::new(),
            pak_containers: Vec::new(),
            ignored_entries: 0,
        };
        let file = input.artifact.open_read()?;
        match input.format {
            UnrealInputFormat::Pak => {
                reject_content_prefix(&input)?;
                self.add_pak_source(
                    &input.input_id,
                    "artifact.pak".to_owned(),
                    file,
                    &mut inspection,
                )?;
            }
            UnrealInputFormat::Zip => {
                reject_content_prefix(&input)?;
                self.open_zip(index, &input.input_id, file, workspace, &mut inspection)?;
            }
            UnrealInputFormat::DirectorySnapshot => {
                let prefix = input.content_prefix.as_deref().ok_or_else(|| {
                    UnrealEngineError::InvalidInput {
                        input_id: input.input_id.clone(),
                        reason: "directory snapshots require content_prefix".to_owned(),
                    }
                })?;
                validate_content_prefix(prefix).map_err(|reason| {
                    UnrealEngineError::InvalidInput {
                        input_id: input.input_id.clone(),
                        reason,
                    }
                })?;
                self.open_snapshot(
                    index,
                    &input.input_id,
                    prefix,
                    file,
                    workspace,
                    &mut inspection,
                )?;
            }
        }
        inspection.files = self
            .candidates
            .iter()
            .filter(|candidate| candidate.input_id == input.input_id)
            .map(|candidate| candidate.relative_path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self.inspections.push(inspection);
        Ok(())
    }

    fn add_pak_source(
        &mut self,
        input_id: &str,
        source_name: String,
        mut file: File,
        inspection: &mut InputInspection,
    ) -> Result<(), UnrealEngineError> {
        let archive = PakArchive::open(&mut file).map_err(|error| UnrealEngineError::Archive {
            input_id: input_id.to_owned(),
            reason: error.to_string(),
        })?;
        if archive.metadata().encrypted_index {
            return Err(UnrealEngineError::InvalidInput {
                input_id: input_id.to_owned(),
                reason: "encrypted PAK indexes require an explicit key contract".to_owned(),
            });
        }
        let metadata = archive.metadata().clone();
        let source_index = self.pak_sources.len();
        let mut local_keys = BTreeSet::new();
        for stored_path in &metadata.files {
            self.bump_entries(input_id, 1)?;
            let relative_path =
                normalize_entry_path(&metadata.mount_point, stored_path).map_err(|error| {
                    UnrealEngineError::InvalidInput {
                        input_id: input_id.to_owned(),
                        reason: error.to_string(),
                    }
                })?;
            self.validate_path_limit(input_id, &relative_path)?;
            let key = relative_path.to_ascii_lowercase();
            if !local_keys.insert(key.clone()) {
                return Err(UnrealEngineError::InvalidInput {
                    input_id: input_id.to_owned(),
                    reason: format!("PAK contains case-colliding path {relative_path}"),
                });
            }
            self.candidates.push(Candidate {
                input_id: input_id.to_owned(),
                relative_path,
                key,
                payload: CandidatePayload::Pak {
                    source_index,
                    stored_path: stored_path.clone(),
                },
            });
        }
        inspection.pak_containers.push(PakContainerInspection {
            source_name,
            metadata,
        });
        self.pak_sources.push(PakSource {
            input_id: input_id.to_owned(),
            file,
            archive,
        });
        Ok(())
    }

    fn open_zip(
        &mut self,
        index: usize,
        input_id: &str,
        file: File,
        workspace: &Path,
        inspection: &mut InputInspection,
    ) -> Result<(), UnrealEngineError> {
        let mut archive = ZipArchive::new(file).map_err(|error| UnrealEngineError::Archive {
            input_id: input_id.to_owned(),
            reason: error.to_string(),
        })?;
        self.bump_entries(input_id, archive.len() as u64)?;
        let mut paths = BTreeMap::new();
        let mut pak_entries = Vec::new();
        let mut expanded = 0_u64;
        for entry_index in 0..archive.len() {
            let entry =
                archive
                    .by_index(entry_index)
                    .map_err(|error| UnrealEngineError::Archive {
                        input_id: input_id.to_owned(),
                        reason: error.to_string(),
                    })?;
            validate_zip_type(&entry).map_err(|reason| UnrealEngineError::InvalidInput {
                input_id: input_id.to_owned(),
                reason,
            })?;
            let path = checked_portable_path(entry.name(), self.limits.max_path_bytes).map_err(
                |reason| UnrealEngineError::InvalidInput {
                    input_id: input_id.to_owned(),
                    reason,
                },
            )?;
            register_archive_path(&mut paths, &path, entry.is_dir()).map_err(|reason| {
                UnrealEngineError::InvalidInput {
                    input_id: input_id.to_owned(),
                    reason: format!("ZIP {reason}"),
                }
            })?;
            if entry.size() > self.limits.max_entry_bytes {
                return Err(UnrealEngineError::LimitExceeded {
                    input_id: input_id.to_owned(),
                    limit: format!("ZIP entry exceeds {} bytes", self.limits.max_entry_bytes),
                });
            }
            expanded = expanded.checked_add(entry.size()).ok_or_else(|| {
                UnrealEngineError::LimitExceeded {
                    input_id: input_id.to_owned(),
                    limit: "ZIP expanded byte count overflow".to_owned(),
                }
            })?;
            if expanded > self.limits.max_expanded_bytes {
                return Err(UnrealEngineError::LimitExceeded {
                    input_id: input_id.to_owned(),
                    limit: format!(
                        "ZIP expanded bytes exceed {}",
                        self.limits.max_expanded_bytes
                    ),
                });
            }
            if entry.size() > 0
                && (entry.compressed_size() == 0
                    || entry.size()
                        > entry
                            .compressed_size()
                            .saturating_mul(self.limits.max_compression_ratio))
            {
                return Err(UnrealEngineError::LimitExceeded {
                    input_id: input_id.to_owned(),
                    limit: "ZIP compression ratio".to_owned(),
                });
            }
            if !entry.is_dir() && has_extension(&path, "pak") {
                pak_entries.push((entry_index, path));
            } else {
                inspection.ignored_entries += 1;
            }
        }
        if pak_entries.len() != 1 {
            return Err(UnrealEngineError::InvalidInput {
                input_id: input_id.to_owned(),
                reason: format!(
                    "ZIP must contain exactly one PAK payload, found {}",
                    pak_entries.len()
                ),
            });
        }
        let (entry_index, source_name) = pak_entries.pop().expect("one PAK entry");
        let output_path = workspace.join(format!("zip-{index}.pak"));
        let mut output = create_new(&output_path, "creating ZIP PAK staging")?;
        let mut entry =
            archive
                .by_index(entry_index)
                .map_err(|error| UnrealEngineError::Archive {
                    input_id: input_id.to_owned(),
                    reason: error.to_string(),
                })?;
        let copied = io::copy(&mut entry, &mut output)
            .map_err(|source| io_error("extracting ZIP PAK payload", source))?;
        if copied != entry.size() {
            return Err(UnrealEngineError::Archive {
                input_id: input_id.to_owned(),
                reason: "ZIP PAK payload ended before its declared size".to_owned(),
            });
        }
        output
            .seek(SeekFrom::Start(0))
            .map_err(|source| io_error("rewinding ZIP PAK payload", source))?;
        self.add_pak_source(input_id, source_name, output, inspection)
    }

    fn open_snapshot(
        &mut self,
        index: usize,
        input_id: &str,
        content_prefix: &str,
        mut file: File,
        workspace: &Path,
        inspection: &mut InputInspection,
    ) -> Result<(), UnrealEngineError> {
        let snapshot = read_directory_snapshot_index(
            &mut file,
            DirectorySnapshotLimits {
                max_entries: self.limits.max_entries,
                max_path_bytes: self.limits.max_path_bytes,
                max_entry_bytes: self.limits.max_entry_bytes,
                max_total_bytes: self.limits.max_expanded_bytes,
            },
        )
        .map_err(|error| match error {
            DirectorySnapshotError::Limit(limit) => UnrealEngineError::LimitExceeded {
                input_id: input_id.to_owned(),
                limit,
            },
            error => UnrealEngineError::Archive {
                input_id: input_id.to_owned(),
                reason: error.to_string(),
            },
        })?;
        self.bump_entries(input_id, snapshot.entries.len() as u64)?;
        let source_index = self.snapshot_sources.len();
        let mut loose = Vec::new();
        let mut nested_paks = Vec::new();
        for entry in snapshot.entries {
            if let Some(relative_path) = content_output_path(content_prefix, &entry.relative_path) {
                loose.push((relative_path, entry.content_offset, entry.size));
            } else if is_nested_pak(&entry.relative_path) {
                nested_paks.push((entry.relative_path, entry.content_offset, entry.size));
            } else {
                inspection.ignored_entries += 1;
            }
        }

        self.snapshot_sources.push(SnapshotSource {
            input_id: input_id.to_owned(),
            file,
        });
        loose.sort_by(|left, right| {
            left.0
                .to_ascii_lowercase()
                .cmp(&right.0.to_ascii_lowercase())
        });
        for (relative_path, offset, size) in loose {
            self.validate_path_limit(input_id, &relative_path)?;
            self.candidates.push(Candidate {
                input_id: input_id.to_owned(),
                key: relative_path.to_ascii_lowercase(),
                relative_path,
                payload: CandidatePayload::Snapshot {
                    source_index,
                    offset,
                    size,
                },
            });
        }

        nested_paks.sort_by_key(|entry| entry.0.to_ascii_lowercase());
        for (nested_index, (source_name, offset, size)) in nested_paks.into_iter().enumerate() {
            let path = workspace.join(format!("snapshot-{index}-{nested_index}.pak"));
            let mut output = create_new(&path, "creating snapshot PAK staging")?;
            let source = &mut self.snapshot_sources[source_index].file;
            source
                .seek(SeekFrom::Start(offset))
                .map_err(|source| io_error("seeking snapshot PAK", source))?;
            let copied = io::copy(&mut Read::by_ref(source).take(size), &mut output)
                .map_err(|source| io_error("extracting snapshot PAK", source))?;
            if copied != size {
                return Err(UnrealEngineError::Archive {
                    input_id: input_id.to_owned(),
                    reason: "snapshot PAK ended before its declared size".to_owned(),
                });
            }
            output
                .seek(SeekFrom::Start(0))
                .map_err(|source| io_error("rewinding snapshot PAK", source))?;
            self.add_pak_source(input_id, source_name, output, inspection)?;
        }
        Ok(())
    }

    fn bump_entries(&mut self, input_id: &str, count: u64) -> Result<(), UnrealEngineError> {
        self.observed_entries = self.observed_entries.checked_add(count).ok_or_else(|| {
            UnrealEngineError::LimitExceeded {
                input_id: input_id.to_owned(),
                limit: "entry count overflow".to_owned(),
            }
        })?;
        if self.observed_entries > self.limits.max_entries {
            return Err(UnrealEngineError::LimitExceeded {
                input_id: input_id.to_owned(),
                limit: format!("entry count exceeds {}", self.limits.max_entries),
            });
        }
        Ok(())
    }

    fn validate_path_limit(&self, input_id: &str, path: &str) -> Result<(), UnrealEngineError> {
        if path.len() > self.limits.max_path_bytes {
            return Err(UnrealEngineError::LimitExceeded {
                input_id: input_id.to_owned(),
                limit: format!("path exceeds {} bytes", self.limits.max_path_bytes),
            });
        }
        Ok(())
    }
}

fn reject_content_prefix(input: &UnrealInput<'_>) -> Result<(), UnrealEngineError> {
    if input.content_prefix.is_some() {
        Err(UnrealEngineError::InvalidInput {
            input_id: input.input_id.clone(),
            reason: "content_prefix is valid only for directory snapshots".to_owned(),
        })
    } else {
        Ok(())
    }
}

fn validate_content_prefix(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Err("content_prefix must be one portable path component".to_owned())
    } else {
        Ok(())
    }
}

pub(crate) fn checked_portable_path(value: &str, max_bytes: usize) -> Result<String, String> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.starts_with(['/', '\\'])
        || value.contains('\\')
        || value.contains(':')
        || value.contains('\0')
    {
        return Err("archive entry path is not a portable relative path".to_owned());
    }
    let path = Path::new(value);
    let mut normalized = String::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err("archive entry path contains traversal".to_owned());
        };
        let component = component
            .to_str()
            .ok_or_else(|| "archive entry path is not UTF-8".to_owned())?;
        if component.is_empty() || component == "." || component == ".." {
            return Err("archive entry path contains traversal".to_owned());
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component);
    }
    if normalized.is_empty() {
        Err("archive entry path is empty".to_owned())
    } else {
        Ok(normalized)
    }
}

fn content_output_path(content_prefix: &str, path: &str) -> Option<String> {
    let mut components = path.split('/');
    if !components.next()?.eq_ignore_ascii_case("content") {
        return None;
    }
    let remainder = components.collect::<Vec<_>>().join("/");
    if remainder.is_empty() {
        None
    } else {
        Some(format!("{content_prefix}/Content/{remainder}"))
    }
}

fn is_nested_pak(path: &str) -> bool {
    path.split('/')
        .next()
        .is_some_and(|component| component.eq_ignore_ascii_case("pak"))
        && has_extension(path, "pak")
}

fn has_extension(path: &str, expected: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

fn register_archive_path(
    paths: &mut BTreeMap<String, bool>,
    path: &str,
    is_directory: bool,
) -> Result<(), String> {
    let key = path.to_ascii_lowercase();
    if paths.contains_key(&key) {
        return Err(format!("contains duplicate or case-colliding path {path}"));
    }
    let components = key.split('/').collect::<Vec<_>>();
    let mut prefix = String::new();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        if paths
            .get(&prefix)
            .is_some_and(|existing_is_directory| !existing_is_directory)
        {
            return Err(format!("file {prefix} collides with child path {path}"));
        }
    }
    if !is_directory {
        let child_prefix = format!("{key}/");
        if paths
            .keys()
            .any(|existing| existing.starts_with(&child_prefix))
        {
            return Err(format!("file {path} collides with an existing child path"));
        }
    }
    paths.insert(key, is_directory);
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

fn create_new(path: &Path, scope: &str) -> Result<File, UnrealEngineError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error(scope, source))
}
