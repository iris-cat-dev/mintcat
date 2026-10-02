mod database;
mod packaging;

pub use database::{KernelDatabase, KernelDatabaseError};
pub use packaging::{packaged_resource_directory, DESKTOP_PRODUCT_NAME};

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PRODUCT_ID: &str = "mintcat-vnext";
pub const LAYOUT_VERSION: u32 = 1;
pub const DATA_DIR_ENV: &str = "MINTCAT_DATA_DIR";
const MARKER_FILE: &str = ".mintcat-vnext.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeEnvironment {
    Development,
    Test,
    Production,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeLayout {
    pub product_id: &'static str,
    pub layout_version: u32,
    pub data_dir: PathBuf,
    pub marker: PathBuf,
    pub kernel_database: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub endpoint: PathBuf,
    pub artifacts_dir: PathBuf,
    pub initialized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    data_dir: PathBuf,
}

#[derive(Debug, Error)]
pub enum RuntimePathError {
    #[error("the vNext data directory must be explicit and non-empty")]
    MissingDataDir,
    #[error("refusing to initialize unmarked directory containing legacy database: {0}")]
    LegacyDatabase(PathBuf),
    #[error("runtime marker at {path} is invalid: {message}")]
    InvalidMarker { path: PathBuf, message: String },
    #[error("runtime marker belongs to {actual}, expected {PRODUCT_ID}")]
    WrongProduct { actual: String },
    #[error("runtime layout version {actual} is incompatible with {LAYOUT_VERSION}")]
    IncompatibleLayout { actual: u32 },
    #[error("runtime path I/O failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeMarker {
    product_id: String,
    layout_version: u32,
    environment: RuntimeEnvironment,
}

impl RuntimePaths {
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self, RuntimePathError> {
        let data_dir = data_dir.into();
        if data_dir.as_os_str().is_empty() {
            return Err(RuntimePathError::MissingDataDir);
        }
        Ok(Self { data_dir })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn layout(&self) -> RuntimeLayout {
        let runtime_dir = self.data_dir.join("runtime");
        RuntimeLayout {
            product_id: PRODUCT_ID,
            layout_version: LAYOUT_VERSION,
            data_dir: self.data_dir.clone(),
            marker: self.data_dir.join(MARKER_FILE),
            kernel_database: self.data_dir.join("kernel.sqlite3"),
            state_dir: self.data_dir.join("state"),
            endpoint: runtime_dir.join("mintcat.sock"),
            runtime_dir,
            artifacts_dir: self.data_dir.join("artifacts"),
            initialized: self.marker_is_compatible().unwrap_or(false),
        }
    }

    pub fn initialize(
        &self,
        environment: RuntimeEnvironment,
    ) -> Result<RuntimeLayout, RuntimePathError> {
        let marker_path = self.data_dir.join(MARKER_FILE);
        if marker_path.exists() {
            self.read_marker()?;
        } else if self.data_dir.join("mintcat.sqlite").exists() {
            return Err(RuntimePathError::LegacyDatabase(self.data_dir.clone()));
        }

        create_dir(&self.data_dir)?;
        create_dir(&self.data_dir.join("runtime"))?;
        create_dir(&self.data_dir.join("artifacts"))?;
        create_dir(&self.data_dir.join("state"))?;

        if !marker_path.exists() {
            let marker = RuntimeMarker {
                product_id: PRODUCT_ID.to_owned(),
                layout_version: LAYOUT_VERSION,
                environment,
            };
            let bytes = serde_json::to_vec_pretty(&marker).expect("runtime marker is serializable");
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker_path)
                .map_err(|source| RuntimePathError::Io {
                    path: marker_path.clone(),
                    source,
                })?;
            file.write_all(&bytes)
                .and_then(|_| file.write_all(b"\n"))
                .and_then(|_| file.sync_all())
                .map_err(|source| RuntimePathError::Io {
                    path: marker_path.clone(),
                    source,
                })?;
        }

        let mut layout = self.layout();
        layout.initialized = true;
        Ok(layout)
    }

    fn marker_is_compatible(&self) -> Result<bool, RuntimePathError> {
        let marker_path = self.data_dir.join(MARKER_FILE);
        if !marker_path.exists() {
            return Ok(false);
        }
        self.read_marker()?;
        Ok(true)
    }

    fn read_marker(&self) -> Result<RuntimeMarker, RuntimePathError> {
        let marker_path = self.data_dir.join(MARKER_FILE);
        let bytes = fs::read(&marker_path).map_err(|source| RuntimePathError::Io {
            path: marker_path.clone(),
            source,
        })?;
        let marker: RuntimeMarker =
            serde_json::from_slice(&bytes).map_err(|error| RuntimePathError::InvalidMarker {
                path: marker_path,
                message: error.to_string(),
            })?;
        if marker.product_id != PRODUCT_ID {
            return Err(RuntimePathError::WrongProduct {
                actual: marker.product_id,
            });
        }
        if marker.layout_version != LAYOUT_VERSION {
            return Err(RuntimePathError::IncompatibleLayout {
                actual: marker.layout_version,
            });
        }
        Ok(marker)
    }
}

fn create_dir(path: &Path) -> Result<(), RuntimePathError> {
    fs::create_dir_all(path).map_err(|source| RuntimePathError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{RuntimeEnvironment, RuntimePathError, RuntimePaths, PRODUCT_ID};

    #[test]
    fn initializes_only_the_explicit_vnext_directory() {
        let sandbox = tempdir().unwrap();
        let legacy = sandbox.path().join("legacy");
        let isolated = sandbox.path().join("vnext");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("mintcat.sqlite"), b"legacy-sentinel").unwrap();

        let layout = RuntimePaths::new(&isolated)
            .unwrap()
            .initialize(RuntimeEnvironment::Test)
            .unwrap();

        assert!(layout.initialized);
        assert_eq!(layout.product_id, PRODUCT_ID);
        assert!(layout.marker.is_file());
        assert!(layout.state_dir.is_dir());
        assert!(layout.runtime_dir.is_dir());
        assert!(layout.artifacts_dir.is_dir());
        assert_eq!(
            fs::read(legacy.join("mintcat.sqlite")).unwrap(),
            b"legacy-sentinel"
        );
        assert!(!legacy.join(".mintcat-vnext.json").exists());
    }

    #[test]
    fn refuses_an_unmarked_legacy_database_directory() {
        let sandbox = tempdir().unwrap();
        fs::write(sandbox.path().join("mintcat.sqlite"), b"legacy").unwrap();

        let error = RuntimePaths::new(sandbox.path())
            .unwrap()
            .initialize(RuntimeEnvironment::Test)
            .unwrap_err();

        assert!(matches!(error, RuntimePathError::LegacyDatabase(_)));
        assert_eq!(
            fs::read(sandbox.path().join("mintcat.sqlite")).unwrap(),
            b"legacy"
        );
        assert!(!sandbox.path().join(".mintcat-vnext.json").exists());
    }

    #[test]
    fn rejects_a_marker_from_another_product() {
        let sandbox = tempdir().unwrap();
        fs::write(
            sandbox.path().join(".mintcat-vnext.json"),
            r#"{"productId":"other","layoutVersion":1,"environment":"test"}"#,
        )
        .unwrap();

        let error = RuntimePaths::new(sandbox.path())
            .unwrap()
            .initialize(RuntimeEnvironment::Test)
            .unwrap_err();

        assert!(matches!(error, RuntimePathError::WrongProduct { .. }));
    }
}
