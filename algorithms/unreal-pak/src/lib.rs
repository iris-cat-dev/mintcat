#[cfg(feature = "legacy-path-api")]
pub mod legacy;

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, Write},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use typed_path::{Utf8Component as _, Utf8UnixPath as PakPath, Utf8UnixPathBuf as PakPathBuf};

#[derive(Debug, Error)]
pub enum PakAlgorithmError {
    #[error("invalid Unreal PAK path {path}: {reason}")]
    InvalidPath { path: String, reason: String },
    #[error("unsupported Unreal PAK build target: {0}")]
    UnsupportedTarget(String),
    #[error("duplicate Unreal PAK output path: {0}")]
    DuplicatePath(String),
    #[error("Unreal PAK operation failed: {0}")]
    Pak(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum PakCodec {
    None,
    Zlib,
    Gzip,
    Oodle,
    Zstd,
    Lz4,
}

impl PakCodec {
    fn from_repak(value: repak::Compression) -> Self {
        match value {
            repak::Compression::Zlib => Self::Zlib,
            repak::Compression::Gzip => Self::Gzip,
            repak::Compression::Oodle => Self::Oodle,
            repak::Compression::Zstd => Self::Zstd,
            repak::Compression::LZ4 => Self::Lz4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PakMetadata {
    pub version: String,
    pub mount_point: String,
    pub encrypted_index: bool,
    pub codecs: Vec<PakCodec>,
    pub files: Vec<String>,
}

pub struct PakArchive {
    reader: repak::PakReader,
    metadata: PakMetadata,
}

impl PakArchive {
    pub fn open(reader: &mut (impl Read + Seek)) -> Result<Self, PakAlgorithmError> {
        let pak = repak::PakBuilder::new()
            .reader(reader)
            .map_err(|error| PakAlgorithmError::Pak(error.to_string()))?;
        let mut files = pak.files();
        files.sort();
        let mut codecs = pak
            .used_compression()
            .into_iter()
            .map(PakCodec::from_repak)
            .collect::<Vec<_>>();
        codecs.sort();
        codecs.dedup();
        let metadata = PakMetadata {
            version: pak.version().to_string(),
            mount_point: pak.mount_point().to_owned(),
            encrypted_index: pak.encrypted_index(),
            codecs,
            files,
        };
        Ok(Self {
            reader: pak,
            metadata,
        })
    }

    pub fn metadata(&self) -> &PakMetadata {
        &self.metadata
    }

    pub fn read_file_to(
        &self,
        stored_path: &str,
        reader: &mut (impl Read + Seek),
        output: &mut impl Write,
    ) -> Result<(), PakAlgorithmError> {
        self.reader
            .read_file(stored_path, reader, output)
            .map_err(|error| PakAlgorithmError::Pak(error.to_string()))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PakBuildCodec {
    None,
    Zlib,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PakBuildTarget {
    pub version: u8,
    pub mount_point: String,
    pub codec: PakBuildCodec,
}

impl Default for PakBuildTarget {
    fn default() -> Self {
        Self {
            version: 11,
            mount_point: "../../../".to_owned(),
            codec: PakBuildCodec::None,
        }
    }
}

#[derive(Debug, Default)]
struct DirectoryCase {
    name: String,
    children: BTreeMap<String, DirectoryCase>,
}

pub struct PakBundleWriter<W: Write + Seek> {
    writer: repak::PakWriter<W>,
    reference_directories: BTreeMap<String, DirectoryCase>,
    compress: bool,
    mount_prefix: String,
    written_paths: BTreeSet<String>,
}

impl<W: Write + Seek> PakBundleWriter<W> {
    pub fn new(
        writer: W,
        reference_paths: &[String],
        target: &PakBuildTarget,
    ) -> Result<Self, PakAlgorithmError> {
        let version = match target.version {
            1 => repak::Version::V1,
            2 => repak::Version::V2,
            3 => repak::Version::V3,
            4 => repak::Version::V4,
            5 => repak::Version::V5,
            6 => repak::Version::V6,
            7 => repak::Version::V7,
            9 => repak::Version::V9,
            10 => repak::Version::V10,
            11 => repak::Version::V11,
            _ => return Err(PakAlgorithmError::UnsupportedTarget(format!(
                "writer requires an unambiguous supported PAK version, requested {}", target.version,
            ))),
        };
        validate_mount_point(&target.mount_point)?;
        let mut mount_prefix = target.mount_point.as_str();
        while let Some(rest) = mount_prefix.strip_prefix("../") {
            mount_prefix = rest;
        }
        let mount_prefix = mount_prefix.trim_end_matches('/').to_owned();
        let mut reference_directories = BTreeMap::new();
        for path in reference_paths {
            let normalized = validate_output_path(path)?;
            let mut directory = &mut reference_directories;
            for component in PakPath::new(&normalized).components() {
                directory = &mut directory
                    .entry(component.as_str().to_ascii_lowercase())
                    .or_insert_with(|| DirectoryCase {
                        name: component.to_string(),
                        children: BTreeMap::new(),
                    })
                    .children;
            }
        }
        let builder = match target.codec {
            PakBuildCodec::None => repak::PakBuilder::new(),
            PakBuildCodec::Zlib => repak::PakBuilder::new().compression([repak::Compression::Zlib]),
        };
        Ok(Self {
            writer: builder.writer(
                writer,
                version,
                target.mount_point.clone(),
                None,
            ),
            reference_directories,
            compress: target.codec == PakBuildCodec::Zlib,
            written_paths: BTreeSet::new(),
            mount_prefix,
        })
    }

    pub fn normalize_case(&self, path: &str) -> Result<PakPathBuf, PakAlgorithmError> {
        let path = validate_output_path(path)?;
        let mut directory = Some(&self.reference_directories);
        let mut normalized = PakPathBuf::new();
        for component in PakPath::new(&path).components() {
            if let Some(reference) =
                directory.and_then(|entries| entries.get(&component.as_str().to_ascii_lowercase()))
            {
                normalized.push(&reference.name);
                directory = Some(&reference.children);
            } else {
                normalized.push(component);
                directory = None;
            }
        }
        Ok(normalized)
    }

    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<String, PakAlgorithmError> {
        let path = self.normalize_case(path)?;
        let key = path.as_str().to_ascii_lowercase();
        if !self.written_paths.insert(key) {
            return Err(PakAlgorithmError::DuplicatePath(path.into_string()));
        }
        let stored_path = if self.mount_prefix.is_empty() {
            path.as_str()
        } else {
            let prefix_len = self.mount_prefix.len();
            if !path.as_str().get(..prefix_len).is_some_and(|prefix| prefix.eq_ignore_ascii_case(&self.mount_prefix))
                || path.as_str().as_bytes().get(prefix_len) != Some(&b'/') {
                return Err(PakAlgorithmError::InvalidPath {
                    path: path.as_str().to_owned(),
                    reason: "output path is outside target mount point".to_owned(),
                });
            }
            &path.as_str()[prefix_len + 1..]
        };
        self.writer
            .write_file(stored_path, self.compress, data)
            .map_err(|error| PakAlgorithmError::Pak(error.to_string()))?;
        Ok(path.into_string())
    }

    pub fn finish(self) -> Result<W, PakAlgorithmError> {
        self.writer
            .write_index()
            .map_err(|error| PakAlgorithmError::Pak(error.to_string()))
    }
}

pub fn normalize_entry_path(
    mount_point: &str,
    stored_path: &str,
) -> Result<String, PakAlgorithmError> {
    validate_mount_point(mount_point)?;
    if stored_path.contains('\\') {
        return Err(invalid_path(stored_path, "backslashes are not portable"));
    }
    let stored_path = validate_output_path(stored_path)?;
    let mut combined = String::new();
    if !mount_point.is_empty() {
        combined.push_str(mount_point.trim_end_matches('/'));
        combined.push('/');
    }
    combined.push_str(&stored_path);
    while let Some(rest) = combined.strip_prefix("../") {
        combined = rest.to_owned();
    }
    validate_output_path(&combined)
}

pub fn validate_output_path(path: &str) -> Result<String, PakAlgorithmError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\0')
        || path.contains('\\')
        || path.contains(':')
    {
        return Err(invalid_path(
            path,
            "path must be a portable relative PAK path",
        ));
    }
    let mut normalized = String::new();
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(invalid_path(
                path,
                "path contains an empty or traversal component",
            ));
        }
        if !normalized.is_empty() {
            normalized.push('/');
        }
        normalized.push_str(component);
    }
    Ok(normalized)
}

fn validate_mount_point(mount_point: &str) -> Result<(), PakAlgorithmError> {
    if mount_point.contains('\0') || mount_point.contains('\\') || mount_point.contains(':') {
        return Err(invalid_path(mount_point, "mount point is not portable"));
    }
    if mount_point.is_empty() {
        return Ok(());
    }
    if !mount_point.ends_with('/') {
        return Err(invalid_path(mount_point, "mount point must end with '/'"));
    }
    let mut saw_normal = false;
    for component in mount_point.trim_end_matches('/').split('/') {
        if component == ".." && !saw_normal {
            continue;
        }
        if component.is_empty() || component == "." || component == ".." {
            return Err(invalid_path(
                mount_point,
                "mount point contains traversal after a named component",
            ));
        }
        saw_normal = true;
    }
    Ok(())
}

fn invalid_path(path: &str, reason: &str) -> PakAlgorithmError {
    PakAlgorithmError::InvalidPath {
        path: path.to_owned(),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn normalizes_mounts_and_reference_case() {
        assert_eq!(
            normalize_entry_path("../../../", "fsd/content/test.uasset").unwrap(),
            "fsd/content/test.uasset"
        );
        assert!(normalize_entry_path("../../../", "../escape").is_err());

        let reference = vec!["FSD/Content/Existing/file.uasset".to_owned()];
        let mut writer = PakBundleWriter::new(
            Cursor::new(Vec::new()),
            &reference,
            &PakBuildTarget::default(),
        )
        .unwrap();
        assert_eq!(
            writer
                .write_file("fsd/content/existing/New.uexp", b"payload")
                .unwrap(),
            "FSD/Content/Existing/New.uexp"
        );
        let bytes = writer.finish().unwrap().into_inner();
        let archive = PakArchive::open(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(archive.metadata().files, ["FSD/Content/Existing/New.uexp"]);
    }
}
