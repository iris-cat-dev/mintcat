use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};
use walkdir::WalkDir;
use zip::ZipArchive;

use crate::{normalize_entry_path, PakArchive};

/// Transitional raw-path adapter for the legacy integrator. New engine APIs consume
/// artifact grants and never accept caller-provided paths.
pub struct UnpackedMod {
    root_path: PathBuf,
    files: HashMap<String, Vec<u8>>,
}

impl UnpackedMod {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let root_path = path.as_ref().to_path_buf();
        if !root_path.is_dir() {
            anyhow::bail!("path is not a directory: {:?}", root_path);
        }
        Ok(Self {
            root_path,
            files: HashMap::new(),
        })
    }

    pub fn is_valid_unpacked_mod(path: impl AsRef<Path>) -> bool {
        let content = path.as_ref().join("Content");
        content.is_dir()
            && WalkDir::new(content)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.file_type().is_file()
                        && entry
                            .path()
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .is_some_and(|extension| {
                                extension.eq_ignore_ascii_case("uasset")
                                    || extension.eq_ignore_ascii_case("uexp")
                            })
                })
    }

    pub fn load_files(&mut self, content_prefix: &str) -> Result<()> {
        self.files.clear();
        validate_prefix(content_prefix)?;
        let content = self.root_path.join("Content");
        if !content.is_dir() {
            anyhow::bail!(
                "Content directory not found in unpacked mod: {:?}",
                self.root_path
            );
        }
        for entry in WalkDir::new(&content).follow_links(false) {
            let entry = entry.context("failed to walk unpacked mod")?;
            if entry.file_type().is_symlink() {
                anyhow::bail!("unpacked mod contains a symbolic link: {:?}", entry.path());
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&content)
                .context("failed to resolve unpacked mod relative path")?;
            let relative = relative
                .to_str()
                .context("unpacked mod path is not UTF-8")?
                .replace('\\', "/");
            let relative =
                checked_portable_path(&relative, 4 * 1024).map_err(anyhow::Error::msg)?;
            let pak_path = format!("{content_prefix}/Content/{relative}");
            let bytes = fs::read(entry.path())
                .with_context(|| format!("failed to read unpacked file: {:?}", entry.path()))?;
            if self.files.insert(pak_path.clone(), bytes).is_some() {
                anyhow::bail!("duplicate unpacked path: {pak_path}");
            }
        }
        if self.files.is_empty() {
            anyhow::bail!("no files found in unpacked mod: {:?}", self.root_path);
        }
        Ok(())
    }

    pub fn files(&self) -> &HashMap<String, Vec<u8>> {
        &self.files
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Vec<u8>)> {
        self.files.iter()
    }

    pub fn get_asset_names(&self) -> Vec<String> {
        let mut names = self
            .files
            .keys()
            .filter_map(|path| path.strip_suffix(".uasset").map(str::to_owned))
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    pub fn get_asset_pair(&self, base_path: &str) -> Option<(Vec<u8>, Vec<u8>)> {
        Some((
            self.files.get(&format!("{base_path}.uasset"))?.clone(),
            self.files.get(&format!("{base_path}.uexp"))?.clone(),
        ))
    }
}

pub type LegacyZipFiles = Vec<(String, Vec<u8>)>;

pub fn read_files_from_zip_by_extension(
    zip_path: &str,
    extension: &str,
) -> Result<LegacyZipFiles, Box<dyn Error>> {
    let file = File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)?;
    let extension = extension.trim_start_matches('.');
    let mut result = Vec::new();
    let mut paths = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let path = checked_portable_path(entry.name(), 4 * 1024).map_err(anyhow::Error::msg)?;
        if !paths.insert(path.to_ascii_lowercase()) {
            return Err(
                anyhow::anyhow!("ZIP contains duplicate or case-colliding path: {path}").into(),
            );
        }
        if entry.is_dir()
            || !Path::new(&path)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        {
            continue;
        }
        let mut contents = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
        entry.read_to_end(&mut contents)?;
        result.push((path, contents));
    }
    result.sort_by_key(|entry| entry.0.to_ascii_lowercase());
    Ok(result)
}

pub fn is_valid_zip_file(path: &str) -> bool {
    File::open(path)
        .ok()
        .and_then(|file| ZipArchive::new(file).ok())
        .is_some()
}

pub fn extract_zip_to_directory(zip_path: &str, output_dir: &str) -> Result<(), Box<dyn Error>> {
    let file = File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)?;
    let output = Path::new(output_dir);
    fs::create_dir_all(output)?;
    let mut paths = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = checked_portable_path(entry.name(), 4 * 1024).map_err(anyhow::Error::msg)?;
        if !paths.insert(relative.to_ascii_lowercase()) {
            return Err(anyhow::anyhow!(
                "ZIP contains duplicate or case-colliding path: {relative}"
            )
            .into());
        }
        if entry.unix_mode().is_some_and(|mode| {
            let file_type = mode & 0o170000;
            file_type != 0 && file_type != 0o100000 && !(file_type == 0o040000 && entry.is_dir())
        }) {
            return Err(anyhow::anyhow!("ZIP contains a link or special file: {relative}").into());
        }
        let destination = output.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        std::io::copy(&mut entry, &mut target)?;
        target.flush()?;
    }
    Ok(())
}

pub fn pak_file_paths(path: impl AsRef<Path>) -> Result<Vec<String>> {
    let path = path.as_ref();
    let mut file = File::open(path).with_context(|| format!("failed to open PAK: {path:?}"))?;
    let archive = PakArchive::open(&mut file).context("failed to parse PAK")?;
    archive
        .metadata()
        .files
        .iter()
        .map(|stored| {
            normalize_entry_path(&archive.metadata().mount_point, stored)
                .map(|path| path.to_ascii_lowercase())
                .map_err(anyhow::Error::from)
        })
        .collect()
}

pub fn directory_mod_file_paths(
    path: impl AsRef<Path>,
    content_prefix: &str,
) -> Result<Vec<String>> {
    validate_prefix(content_prefix)?;
    let root = path.as_ref();
    let mut paths = BTreeSet::new();
    let content = root.join("Content");
    if content.is_dir() {
        for entry in WalkDir::new(&content).follow_links(false) {
            let entry = entry.context("failed to walk unpacked mod")?;
            if entry.file_type().is_symlink() {
                anyhow::bail!("unpacked mod contains a symbolic link: {:?}", entry.path());
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&content)?
                .to_str()
                .context("unpacked path is not UTF-8")?
                .replace('\\', "/");
            let relative =
                checked_portable_path(&relative, 4 * 1024).map_err(anyhow::Error::msg)?;
            paths.insert(format!(
                "{}/content/{}",
                content_prefix.to_ascii_lowercase(),
                relative.to_ascii_lowercase()
            ));
        }
    }
    let pak_directory = root.join("pak");
    if pak_directory.is_dir() {
        let mut paks = fs::read_dir(&pak_directory)?.collect::<Result<Vec<_>, _>>()?;
        paks.sort_by_key(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase());
        for entry in paks {
            if entry.file_type()?.is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pak"))
            {
                paths.extend(pak_file_paths(entry.path())?);
            }
        }
    }
    Ok(paths.into_iter().collect())
}

fn validate_prefix(prefix: &str) -> Result<()> {
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        anyhow::bail!("content prefix must be one portable path component");
    }
    Ok(())
}

fn checked_portable_path(value: &str, max_bytes: usize) -> Result<String, String> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.starts_with(['/', '\\'])
        || value.contains('\\')
        || value.contains(':')
        || value.contains('\0')
    {
        return Err("archive entry path is not a portable relative path".to_owned());
    }
    let mut normalized = String::new();
    for component in Path::new(value).components() {
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
