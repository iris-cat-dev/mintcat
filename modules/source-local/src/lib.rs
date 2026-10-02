use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use mintcat_runtime_artifacts::{ArtifactError, ArtifactStore};
use mintcat_runtime_broker::{
    interaction_binding, BrokerError, DirectorySelectionRequest, DirectorySnapshot,
    FileSelectionRequest, InputSnapshotLimits, InteractionClient, InteractionContext,
};
use mintcat_sdk::{
    ActionRequired, ArtifactRef, ModRef, DIRECTORY_SNAPSHOT_MAGIC, DIRECTORY_SNAPSHOT_MEDIA_TYPE,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use zip::ZipArchive;

pub const SOURCE_ID: &str = "local";
const ALLOWED_FILE_EXTENSIONS: &[&str] = &["pak", "zip", "js", "dll", "exe", "so", "dylib"];

#[derive(Debug, Clone, Copy)]
pub struct LocalImportLimits {
    pub max_input_bytes: u64,
    pub max_entries: u64,
    pub max_entry_bytes: u64,
    pub max_expanded_bytes: u64,
    pub max_compression_ratio: u64,
    pub max_path_depth: u32,
}

impl Default for LocalImportLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 2 * 1024 * 1024 * 1024,
            max_entries: 20_000,
            max_entry_bytes: 2 * 1024 * 1024 * 1024,
            max_expanded_bytes: 8 * 1024 * 1024 * 1024,
            max_compression_ratio: 1_000,
            max_path_depth: 64,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LocalInputKind {
    Pak,
    ZipArchive,
    JavaScript,
    NativeBinary,
    UnpackedDirectory,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum LocalContentKind {
    UnrealAssets,
    Pak,
    ZipArchive,
    JavaScript,
    NativeBinary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalInputRisk {
    pub script_files: u64,
    pub native_binary_files: u64,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalImport {
    pub mod_ref: ModRef,
    pub display_name: String,
    pub input_kind: LocalInputKind,
    pub content_kinds: Vec<LocalContentKind>,
    pub risk: LocalInputRisk,
    pub entry_count: u64,
    pub expanded_bytes: u64,
    pub artifact: ArtifactRef,
}

#[derive(Debug, Error)]
pub enum LocalSourceError {
    #[error(transparent)]
    Broker(#[from] BrokerError),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error("local input changed while its immutable snapshot was created")]
    InputChanged,
    #[error("local input I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("unsupported local input: {0}")]
    Unsupported(String),
    #[error("unsafe local input path: {0}")]
    UnsafePath(String),
    #[error("local input exceeds its resource limit: {0}")]
    ResourceLimit(String),
    #[error("local archive is invalid: {0}")]
    InvalidArchive(String),
}

#[derive(Clone)]
pub struct LocalSource {
    artifacts: ArtifactStore,
    interactions: InteractionClient,
    staging: PathBuf,
    limits: LocalImportLimits,
}

impl LocalSource {
    pub fn new(
        artifacts: ArtifactStore,
        interactions: InteractionClient,
        staging: impl AsRef<Path>,
        limits: LocalImportLimits,
    ) -> Result<Self, LocalSourceError> {
        validate_limits(limits)?;
        fs::create_dir_all(staging.as_ref())?;
        let staging = staging.as_ref().canonicalize()?;
        if !fs::symlink_metadata(&staging)?.is_dir() {
            return Err(LocalSourceError::UnsafePath(
                "local import staging is not a directory".to_owned(),
            ));
        }
        Ok(Self {
            artifacts,
            interactions,
            staging,
            limits,
        })
    }

    pub fn request_file_input(
        &self,
        job_id: &str,
        context: &InteractionContext,
        purpose: impl Into<String>,
    ) -> Result<ActionRequired, LocalSourceError> {
        Ok(self.interactions.request_file_selection(
            job_id,
            context,
            FileSelectionRequest {
                purpose: purpose.into(),
                allowed_extensions: ALLOWED_FILE_EXTENSIONS
                    .iter()
                    .map(|extension| (*extension).to_owned())
                    .collect(),
            },
        )?)
    }

    pub fn request_directory_input(
        &self,
        job_id: &str,
        context: &InteractionContext,
        purpose: impl Into<String>,
    ) -> Result<ActionRequired, LocalSourceError> {
        Ok(self.interactions.request_directory_selection(
            job_id,
            context,
            DirectorySelectionRequest {
                purpose: purpose.into(),
            },
        )?)
    }

    pub fn import_file(
        &self,
        action: &ActionRequired,
        remote_game_key: &str,
    ) -> Result<LocalImport, LocalSourceError> {
        validate_game_key(remote_game_key)?;
        let mut selected = self
            .interactions
            .consume_selected_file(&action.action_id, &interaction_binding(action))?;
        if selected.size() > self.limits.max_input_bytes {
            return Err(LocalSourceError::ResourceLimit(
                "input file bytes".to_owned(),
            ));
        }
        let file_name = selected.file_name().to_owned();
        let input_kind = classify_root_file(&file_name)?;
        let media_type = media_type(input_kind).to_owned();
        let selected_size = selected.size();
        let mut limited =
            Read::by_ref(selected.file_mut()).take(self.limits.max_input_bytes.saturating_add(1));
        let artifact = self
            .artifacts
            .publish_blob_reader(&mut limited, Some(media_type))?;
        if artifact.size > self.limits.max_input_bytes {
            return Err(LocalSourceError::ResourceLimit(
                "input file bytes".to_owned(),
            ));
        }
        if artifact.size != selected_size {
            return Err(LocalSourceError::InputChanged);
        }
        let grant = self.artifacts.grant_blob(&artifact)?;
        let artifact_path = Path::new(&grant.descriptor.path);
        let inspection = match input_kind {
            LocalInputKind::ZipArchive => inspect_zip(artifact_path, self.limits)?,
            LocalInputKind::Pak => Inspection::single(LocalContentKind::Pak, artifact.size),
            LocalInputKind::JavaScript => {
                Inspection::single(LocalContentKind::JavaScript, artifact.size)
            }
            LocalInputKind::NativeBinary => {
                Inspection::single(LocalContentKind::NativeBinary, artifact.size)
            }
            LocalInputKind::UnpackedDirectory => {
                unreachable!("directory input uses import_directory")
            }
        };
        Ok(build_import(
            remote_game_key,
            display_name(&file_name),
            input_kind,
            inspection,
            artifact,
        ))
    }

    pub fn import_directory(
        &self,
        action: &ActionRequired,
        remote_game_key: &str,
    ) -> Result<LocalImport, LocalSourceError> {
        validate_game_key(remote_game_key)?;
        let selected = self
            .interactions
            .consume_selected_directory(&action.action_id, &interaction_binding(action))?;
        let display_name = selected.directory_name().to_owned();
        let workspace = self
            .staging
            .join(format!("directory-import-{}", Uuid::new_v4()));
        fs::create_dir(&workspace)?;
        let result = (|| {
            let snapshot_root = workspace.join("snapshot");
            let snapshot = selected.snapshot_to(
                &snapshot_root,
                InputSnapshotLimits {
                    max_entries: self.limits.max_entries,
                    max_file_bytes: self.limits.max_entry_bytes,
                    max_total_bytes: self.limits.max_expanded_bytes,
                    max_depth: self.limits.max_path_depth,
                },
            )?;
            let inspection = inspect_directory(&snapshot)?;
            if inspection.content_kinds.is_empty() {
                return Err(LocalSourceError::Unsupported(
                    "directory contains no supported mod content".to_owned(),
                ));
            }
            let bundle = workspace.join("snapshot.mintcat-directory");
            write_directory_bundle(&snapshot_root, &snapshot, &bundle)?;
            let artifact = self
                .artifacts
                .publish_blob_file(&bundle, Some(DIRECTORY_SNAPSHOT_MEDIA_TYPE.to_owned()))?;
            Ok(build_import(
                remote_game_key,
                display_name,
                LocalInputKind::UnpackedDirectory,
                inspection,
                artifact,
            ))
        })();
        let cleanup = fs::remove_dir_all(&workspace);
        match (result, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error.into()),
        }
    }
}

#[derive(Debug)]
struct Inspection {
    content_kinds: Vec<LocalContentKind>,
    risk: LocalInputRisk,
    entry_count: u64,
    expanded_bytes: u64,
}

impl Inspection {
    fn single(kind: LocalContentKind, size: u64) -> Self {
        let mut risk = LocalInputRisk::default();
        record_risk(kind, &mut risk);
        Self {
            content_kinds: vec![kind],
            risk,
            entry_count: 1,
            expanded_bytes: size,
        }
    }
}

fn validate_limits(limits: LocalImportLimits) -> Result<(), LocalSourceError> {
    if limits.max_input_bytes == 0
        || limits.max_entries == 0
        || limits.max_entry_bytes == 0
        || limits.max_expanded_bytes == 0
        || limits.max_compression_ratio == 0
        || limits.max_path_depth == 0
    {
        return Err(LocalSourceError::ResourceLimit(
            "all local import limits must be positive".to_owned(),
        ));
    }
    Ok(())
}

fn validate_game_key(value: &str) -> Result<(), LocalSourceError> {
    if value.trim().is_empty() {
        return Err(LocalSourceError::Unsupported(
            "local imports require an explicit game binding".to_owned(),
        ));
    }
    Ok(())
}

fn classify_root_file(file_name: &str) -> Result<LocalInputKind, LocalSourceError> {
    match extension(file_name).as_deref() {
        Some("pak") => Ok(LocalInputKind::Pak),
        Some("zip") => Ok(LocalInputKind::ZipArchive),
        Some("js") => Ok(LocalInputKind::JavaScript),
        Some("dll" | "exe" | "so" | "dylib") => Ok(LocalInputKind::NativeBinary),
        _ => Err(LocalSourceError::Unsupported(file_name.to_owned())),
    }
}

fn media_type(kind: LocalInputKind) -> &'static str {
    match kind {
        LocalInputKind::Pak => "application/vnd.unreal.pak",
        LocalInputKind::ZipArchive => "application/zip",
        LocalInputKind::JavaScript => "text/javascript",
        LocalInputKind::NativeBinary => "application/x-native-binary",
        LocalInputKind::UnpackedDirectory => DIRECTORY_SNAPSHOT_MEDIA_TYPE,
    }
}

fn display_name(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(file_name)
        .to_owned()
}

fn extension(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
}

fn inspect_directory(snapshot: &DirectorySnapshot) -> Result<Inspection, LocalSourceError> {
    let mut content_kinds = BTreeMap::new();
    let mut risk = LocalInputRisk::default();
    for entry in &snapshot.entries {
        if let Some(kind) = classify_content_path(&entry.relative_path) {
            content_kinds.insert(kind, ());
            record_risk(kind, &mut risk);
        }
    }
    Ok(Inspection {
        content_kinds: content_kinds.into_keys().collect(),
        risk,
        entry_count: u64::try_from(snapshot.entries.len())
            .map_err(|_| LocalSourceError::ResourceLimit("entry count".to_owned()))?,
        expanded_bytes: snapshot.total_bytes,
    })
}

fn inspect_zip(path: &Path, limits: LocalImportLimits) -> Result<Inspection, LocalSourceError> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| LocalSourceError::InvalidArchive(error.to_string()))?;
    let entry_count = u64::try_from(archive.len())
        .map_err(|_| LocalSourceError::ResourceLimit("archive entry count".to_owned()))?;
    if entry_count > limits.max_entries {
        return Err(LocalSourceError::ResourceLimit(
            "archive entry count".to_owned(),
        ));
    }
    let mut expanded_bytes = 0_u64;
    let mut case_paths = BTreeMap::new();
    let mut content_kinds = BTreeMap::new();
    let mut risk = LocalInputRisk::default();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| LocalSourceError::InvalidArchive(error.to_string()))?;
        validate_zip_entry_type(&entry)?;
        let relative = checked_archive_path(entry.name(), limits.max_path_depth)?;
        validate_archive_collision(&mut case_paths, &relative, entry.is_dir())?;
        if entry.is_dir() {
            continue;
        }
        if entry.size() > limits.max_entry_bytes {
            return Err(LocalSourceError::ResourceLimit(
                "archive entry bytes".to_owned(),
            ));
        }
        expanded_bytes = expanded_bytes
            .checked_add(entry.size())
            .ok_or_else(|| LocalSourceError::ResourceLimit("expanded bytes".to_owned()))?;
        if expanded_bytes > limits.max_expanded_bytes {
            return Err(LocalSourceError::ResourceLimit("expanded bytes".to_owned()));
        }
        if entry.size() > 0
            && (entry.compressed_size() == 0
                || entry.size()
                    > entry
                        .compressed_size()
                        .saturating_mul(limits.max_compression_ratio))
        {
            return Err(LocalSourceError::ResourceLimit(
                "archive compression ratio".to_owned(),
            ));
        }
        let copied = io::copy(&mut entry, &mut io::sink())?;
        if copied != entry.size() {
            return Err(LocalSourceError::InvalidArchive(
                "archive entry ended before its declared size".to_owned(),
            ));
        }
        if let Some(kind) = classify_content_path(&relative) {
            content_kinds.insert(kind, ());
            record_risk(kind, &mut risk);
        }
    }
    Ok(Inspection {
        content_kinds: content_kinds.into_keys().collect(),
        risk,
        entry_count,
        expanded_bytes,
    })
}
fn validate_archive_collision(
    paths: &mut BTreeMap<String, (String, bool)>,
    relative: &str,
    is_directory: bool,
) -> Result<(), LocalSourceError> {
    let key = relative.to_ascii_lowercase();
    if paths.contains_key(&key) {
        return Err(LocalSourceError::UnsafePath(
            "archive has duplicate or case-colliding entries".to_owned(),
        ));
    }
    let mut prefix = String::new();
    let components = key.split('/').collect::<Vec<_>>();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        if paths
            .get(&prefix)
            .is_some_and(|(_, existing_is_directory)| !existing_is_directory)
        {
            return Err(LocalSourceError::UnsafePath(
                "archive file collides with a child path".to_owned(),
            ));
        }
    }
    if !is_directory {
        let child_prefix = format!("{key}/");
        if paths
            .keys()
            .any(|existing| existing.starts_with(&child_prefix))
        {
            return Err(LocalSourceError::UnsafePath(
                "archive file collides with a directory path".to_owned(),
            ));
        }
    }
    paths.insert(key, (relative.to_owned(), is_directory));
    Ok(())
}

fn validate_zip_entry_type(entry: &zip::read::ZipFile<'_>) -> Result<(), LocalSourceError> {
    let Some(mode) = entry.unix_mode() else {
        return Ok(());
    };
    let file_type = mode & 0o170000;
    if file_type == 0 || file_type == 0o100000 || (file_type == 0o040000 && entry.is_dir()) {
        return Ok(());
    }
    Err(LocalSourceError::UnsafePath(
        "archive contains a link or special file".to_owned(),
    ))
}

fn checked_archive_path(value: &str, max_depth: u32) -> Result<String, LocalSourceError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || value
            .as_bytes()
            .first()
            .is_some_and(|byte| matches!(*byte, b'/' | b'\\'))
    {
        return Err(LocalSourceError::UnsafePath(
            "archive entry is not relative".to_owned(),
        ));
    }
    let mut normalized = String::new();
    let mut depth = 0_u32;
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(LocalSourceError::UnsafePath(
                "archive entry contains traversal".to_owned(),
            ));
        };
        let component = component.to_str().ok_or_else(|| {
            LocalSourceError::UnsafePath("archive path is not valid UTF-8".to_owned())
        })?;
        if component.is_empty() || component.bytes().any(|byte| matches!(byte, b':' | b'\\')) {
            return Err(LocalSourceError::UnsafePath(
                "archive entry is not a portable path".to_owned(),
            ));
        }
        depth = depth
            .checked_add(1)
            .ok_or_else(|| LocalSourceError::ResourceLimit("archive path depth".to_owned()))?;
        if depth > max_depth {
            return Err(LocalSourceError::ResourceLimit(
                "archive path depth".to_owned(),
            ));
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component);
    }
    if normalized.is_empty() {
        return Err(LocalSourceError::UnsafePath(
            "archive entry path is empty".to_owned(),
        ));
    }
    Ok(normalized)
}

fn classify_content_path(path: &str) -> Option<LocalContentKind> {
    let extension = extension(path)?;
    if matches!(extension.as_str(), "uasset" | "uexp" | "ubulk")
        && path
            .split('/')
            .any(|component| component.eq_ignore_ascii_case("Content"))
    {
        return Some(LocalContentKind::UnrealAssets);
    }
    match extension.as_str() {
        "pak" => Some(LocalContentKind::Pak),
        "zip" => Some(LocalContentKind::ZipArchive),
        "js" => Some(LocalContentKind::JavaScript),
        "dll" | "exe" | "so" | "dylib" => Some(LocalContentKind::NativeBinary),
        _ => None,
    }
}

fn record_risk(kind: LocalContentKind, risk: &mut LocalInputRisk) {
    match kind {
        LocalContentKind::JavaScript => risk.script_files += 1,
        LocalContentKind::NativeBinary => risk.native_binary_files += 1,
        LocalContentKind::UnrealAssets | LocalContentKind::Pak | LocalContentKind::ZipArchive => {}
    }
    risk.requires_approval = risk.script_files > 0 || risk.native_binary_files > 0;
}

fn write_directory_bundle(
    root: &Path,
    snapshot: &DirectorySnapshot,
    destination: &Path,
) -> Result<(), LocalSourceError> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    output.write_all(DIRECTORY_SNAPSHOT_MAGIC)?;
    output.write_all(
        &u64::try_from(snapshot.entries.len())
            .map_err(|_| LocalSourceError::ResourceLimit("entry count".to_owned()))?
            .to_be_bytes(),
    )?;
    for entry in &snapshot.entries {
        let path = checked_bundle_path(&entry.relative_path)?;
        let path_bytes = entry.relative_path.as_bytes();
        output.write_all(
            &u32::try_from(path_bytes.len())
                .map_err(|_| LocalSourceError::ResourceLimit("path bytes".to_owned()))?
                .to_be_bytes(),
        )?;
        output.write_all(path_bytes)?;
        output.write_all(&entry.size.to_be_bytes())?;
        let mut input = File::open(root.join(path))?;
        let copied = io::copy(&mut Read::by_ref(&mut input).take(entry.size), &mut output)?;
        if copied != entry.size {
            return Err(LocalSourceError::InvalidArchive(
                "directory snapshot file changed after capture".to_owned(),
            ));
        }
        let mut trailing = [0_u8; 1];
        if input.read(&mut trailing)? != 0 {
            return Err(LocalSourceError::InvalidArchive(
                "directory snapshot file grew after capture".to_owned(),
            ));
        }
    }
    output.sync_all()?;
    Ok(())
}

fn checked_bundle_path(value: &str) -> Result<PathBuf, LocalSourceError> {
    let path = Path::new(value);
    if value.is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(LocalSourceError::UnsafePath(
            "directory snapshot entry is not relative".to_owned(),
        ));
    }
    Ok(path.to_path_buf())
}

fn build_import(
    remote_game_key: &str,
    display_name: String,
    input_kind: LocalInputKind,
    inspection: Inspection,
    artifact: ArtifactRef,
) -> LocalImport {
    let digest = artifact.digest.clone();
    LocalImport {
        mod_ref: ModRef {
            source_id: SOURCE_ID.to_owned(),
            remote_game_key: remote_game_key.to_owned(),
            remote_mod_id: digest.clone(),
            version_id: Some(digest.clone()),
            remote_file_id: Some(digest),
        },
        display_name,
        input_kind,
        content_kinds: inspection.content_kinds,
        risk: inspection.risk,
        entry_count: inspection.entry_count,
        expanded_bytes: inspection.expanded_bytes,
        artifact,
    }
}

#[cfg(test)]
mod tests;
