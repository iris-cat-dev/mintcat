use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use mintcat_game_api::{
    DiscoveryConfidence, GameInstallationCandidate, RunningProcess, ValidatedGameInstallation,
};
use mintcat_unreal_pak::{normalize_entry_path, PakArchive};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    descriptor::{
        descriptor, STEAM_APP_ID, STEAM_MAIN_PAK, STEAM_VARIANT_ID, XBOX_MAIN_PAK, XBOX_VARIANT_ID,
    },
    DrgError, DRG_GAME_ID,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DrgVariant {
    Steam,
    Xbox,
}

impl DrgVariant {
    pub fn id(self) -> &'static str {
        match self {
            Self::Steam => STEAM_VARIANT_ID,
            Self::Xbox => XBOX_VARIANT_ID,
        }
    }

    pub fn main_pak(self) -> &'static str {
        match self {
            Self::Steam => STEAM_MAIN_PAK,
            Self::Xbox => XBOX_MAIN_PAK,
        }
    }

    pub fn from_id(value: &str) -> Result<Self, DrgError> {
        match value {
            STEAM_VARIANT_ID => Ok(Self::Steam),
            XBOX_VARIANT_ID => Ok(Self::Xbox),
            _ => Err(DrgError::UnsupportedLayout(format!(
                "unknown DRG variant {value}"
            ))),
        }
    }

    pub fn from_main_pak(path: &Path) -> Result<Self, DrgError> {
        match path.file_name().and_then(|name| name.to_str()) {
            Some("FSD-WindowsNoEditor.pak") => Ok(Self::Steam),
            Some("FSD-WinGDK.pak") => Ok(Self::Xbox),
            Some(name) => Err(DrgError::UnsupportedLayout(format!(
                "unsupported DRG main PAK {name}"
            ))),
            None => Err(DrgError::UnsupportedLayout(
                "DRG main PAK filename is not UTF-8".to_owned(),
            )),
        }
    }
}

pub fn discover_steam() -> Result<Vec<GameInstallationCandidate>, DrgError> {
    let app_id = STEAM_APP_ID
        .parse::<u32>()
        .expect("static Steam app ID is numeric");
    let Some(root) = steamlocate::SteamDir::locate().ok().and_then(|steam| {
        steam
            .find_app(app_id)
            .ok()
            .flatten()
            .map(|(app, library)| library.resolve_app_dir(&app))
    }) else {
        return Ok(Vec::new());
    };
    discover_roots([(root, DiscoveryConfidence::StoreManifest)])
}

pub fn discover_roots(
    roots: impl IntoIterator<Item = (PathBuf, DiscoveryConfidence)>,
) -> Result<Vec<GameInstallationCandidate>, DrgError> {
    let mut candidates = Vec::new();
    for (root, confidence) in roots {
        for variant in [DrgVariant::Steam, DrgVariant::Xbox] {
            let main_pak = root.join(variant.main_pak());
            let metadata = match fs::symlink_metadata(&main_pak) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(DrgError::Io {
                        scope: format!("inspecting DRG candidate {}", main_pak.display()),
                        source,
                    });
                }
            };
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                continue;
            }
            let canonical_root = root.canonicalize().map_err(|source| DrgError::Io {
                scope: format!("canonicalizing DRG root {}", root.display()),
                source,
            })?;
            candidates.push(GameInstallationCandidate {
                game_id: DRG_GAME_ID.to_owned(),
                variant_id: variant.id().to_owned(),
                root_path: canonical_root.to_string_lossy().into_owned(),
                main_pak_path: canonical_root
                    .join(variant.main_pak())
                    .to_string_lossy()
                    .into_owned(),
                confidence,
                evidence: vec![format!("found regular file {}", variant.main_pak())],
            });
        }
    }
    candidates.sort_by(|left, right| {
        left.root_path
            .cmp(&right.root_path)
            .then_with(|| left.variant_id.cmp(&right.variant_id))
    });
    candidates.dedup_by(|left, right| {
        left.root_path == right.root_path && left.variant_id == right.variant_id
    });
    Ok(candidates)
}

pub fn validate_installation(
    root: &Path,
    variant: DrgVariant,
) -> Result<ValidatedGameInstallation, DrgError> {
    let canonical_root = root.canonicalize().map_err(|source| DrgError::Io {
        scope: format!("canonicalizing DRG root {}", root.display()),
        source,
    })?;
    let root_metadata = fs::symlink_metadata(&canonical_root).map_err(|source| DrgError::Io {
        scope: format!("inspecting DRG root {}", canonical_root.display()),
        source,
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(DrgError::UnsupportedLayout(
            "DRG root must resolve to a real directory".to_owned(),
        ));
    }
    let variant_descriptor = descriptor()
        .variants
        .into_iter()
        .find(|candidate| candidate.variant_id == variant.id())
        .expect("descriptor contains every DrgVariant");
    let main_pak = canonical_root.join(&variant_descriptor.main_pak_relative_path);
    let metadata = fs::symlink_metadata(&main_pak).map_err(|source| DrgError::Io {
        scope: format!("inspecting DRG main PAK {}", main_pak.display()),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DrgError::UnsupportedLayout(format!(
            "{} is not a regular DRG main PAK",
            main_pak.display()
        )));
    }
    for relative in [
        &variant_descriptor.paks_relative_path,
        &variant_descriptor.binaries_relative_path,
    ] {
        let directory = canonical_root.join(relative);
        if !directory.is_dir() {
            return Err(DrgError::UnsupportedLayout(format!(
                "required DRG directory is absent: {relative}"
            )));
        }
    }

    let mut pak_file = File::open(&main_pak).map_err(|source| DrgError::Io {
        scope: "opening DRG main PAK".to_owned(),
        source,
    })?;
    let archive = PakArchive::open(&mut pak_file)?;
    if archive.metadata().encrypted_index {
        return Err(DrgError::UnsupportedVersion(
            "encrypted DRG main PAK indexes are unsupported".to_owned(),
        ));
    }
    let mut has_asset_registry = false;
    for stored in &archive.metadata().files {
        let path = normalize_entry_path(&archive.metadata().mount_point, stored)?;
        if path.eq_ignore_ascii_case("FSD/AssetRegistry.bin") {
            has_asset_registry = true;
            break;
        }
    }
    if !has_asset_registry {
        return Err(DrgError::UnsupportedVersion(
            "DRG main PAK does not contain FSD/AssetRegistry.bin".to_owned(),
        ));
    }
    let fingerprint = hash_file(&main_pak)?;
    Ok(ValidatedGameInstallation {
        game_id: DRG_GAME_ID.to_owned(),
        variant_id: variant.id().to_owned(),
        root_path: canonical_root.to_string_lossy().into_owned(),
        main_pak_relative_path: variant_descriptor.main_pak_relative_path,
        fingerprint,
        target: variant_descriptor.target,
        main_pak_size: metadata.len(),
    })
}

pub fn validate_main_pak_path(path: &Path) -> Result<(PathBuf, DrgVariant), DrgError> {
    let variant = DrgVariant::from_main_pak(path)?;
    let suffix_components = Path::new(variant.main_pak()).components().count();
    let mut root = path;
    for _ in 0..suffix_components {
        root = root.parent().ok_or_else(|| {
            DrgError::UnsupportedLayout(format!(
                "main PAK is not beneath the expected {} layout",
                variant.main_pak()
            ))
        })?;
    }
    let expected = root.join(variant.main_pak());
    if expected != path {
        return Err(DrgError::UnsupportedLayout(format!(
            "main PAK path must end with {}",
            variant.main_pak()
        )));
    }
    Ok((root.to_path_buf(), variant))
}

pub fn ensure_not_running(
    variant: DrgVariant,
    processes: &[RunningProcess],
) -> Result<(), DrgError> {
    let expected = descriptor()
        .variants
        .into_iter()
        .find(|candidate| candidate.variant_id == variant.id())
        .expect("descriptor contains every DrgVariant")
        .process_names;
    let matches = processes
        .iter()
        .filter(|process| {
            expected
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&process.executable_name))
        })
        .map(|process| format!("{} (pid {})", process.executable_name, process.pid))
        .collect::<Vec<_>>();
    if matches.is_empty() {
        Ok(())
    } else {
        Err(DrgError::GameRunning(matches.join(", ")))
    }
}

fn hash_file(path: &Path) -> Result<String, DrgError> {
    let mut file = File::open(path).map_err(|source| DrgError::Io {
        scope: format!("opening {} for fingerprint", path.display()),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| DrgError::Io {
            scope: format!("hashing {}", path.display()),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}
