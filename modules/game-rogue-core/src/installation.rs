use std::{
    fs::{self, File},
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

use mintcat_game_api::{
    DiscoveryConfidence, GameInstallationCandidate, RunningProcess, ValidatedGameInstallation,
};
use mintcat_integrator_core::uasset_utils::asset_registry::{AssetRegistry, Readable as _};
use mintcat_unreal_pak::{normalize_entry_path, PakArchive};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    descriptor::{
        descriptor, MAIN_PAK, PLAYTEST_APP_ID, PLAYTEST_VARIANT_ID, RELEASE_APP_ID,
        RELEASE_VARIANT_ID,
    },
    RogueCoreError, ROGUE_CORE_GAME_ID,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RogueCoreVariant {
    SteamRelease,
    SteamPlaytest,
}

impl RogueCoreVariant {
    pub fn id(self) -> &'static str {
        match self {
            Self::SteamRelease => RELEASE_VARIANT_ID,
            Self::SteamPlaytest => PLAYTEST_VARIANT_ID,
        }
    }

    pub fn app_id(self) -> &'static str {
        match self {
            Self::SteamRelease => RELEASE_APP_ID,
            Self::SteamPlaytest => PLAYTEST_APP_ID,
        }
    }

    pub fn from_id(value: &str) -> Result<Self, RogueCoreError> {
        match value {
            RELEASE_VARIANT_ID => Ok(Self::SteamRelease),
            PLAYTEST_VARIANT_ID => Ok(Self::SteamPlaytest),
            _ => Err(RogueCoreError::UnsupportedLayout(format!(
                "unknown Rogue Core variant {value}"
            ))),
        }
    }
}

pub fn discover_steam() -> Result<Vec<GameInstallationCandidate>, RogueCoreError> {
    let Ok(steam) = steamlocate::SteamDir::locate() else {
        return Ok(Vec::new());
    };
    let mut roots = Vec::new();
    for variant in [
        RogueCoreVariant::SteamRelease,
        RogueCoreVariant::SteamPlaytest,
    ] {
        let app_id = variant
            .app_id()
            .parse::<u32>()
            .expect("static Steam app ID is numeric");
        if let Some((app, library)) = steam
            .find_app(app_id)
            .map_err(|error| RogueCoreError::UnsupportedLayout(error.to_string()))?
        {
            roots.push((
                library.resolve_app_dir(&app).join("RogueCore"),
                variant,
                DiscoveryConfidence::StoreManifest,
            ));
        }
    }
    discover_roots(roots)
}

pub fn discover_roots(
    roots: impl IntoIterator<Item = (PathBuf, RogueCoreVariant, DiscoveryConfidence)>,
) -> Result<Vec<GameInstallationCandidate>, RogueCoreError> {
    let mut candidates = Vec::new();
    for (root, variant, confidence) in roots {
        let main_pak = root.join(MAIN_PAK);
        let metadata = match fs::symlink_metadata(&main_pak) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(RogueCoreError::Io {
                    scope: format!("inspecting Rogue Core candidate {}", main_pak.display()),
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let canonical_root = root.canonicalize().map_err(|source| RogueCoreError::Io {
            scope: format!("canonicalizing Rogue Core root {}", root.display()),
            source,
        })?;
        candidates.push(GameInstallationCandidate {
            game_id: ROGUE_CORE_GAME_ID.to_owned(),
            variant_id: variant.id().to_owned(),
            root_path: canonical_root.to_string_lossy().into_owned(),
            main_pak_path: canonical_root.join(MAIN_PAK).to_string_lossy().into_owned(),
            confidence,
            evidence: vec![format!(
                "found regular {MAIN_PAK} for Steam app {}",
                variant.app_id()
            )],
        });
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
    variant: RogueCoreVariant,
) -> Result<ValidatedGameInstallation, RogueCoreError> {
    let canonical_root = root.canonicalize().map_err(|source| RogueCoreError::Io {
        scope: format!("canonicalizing Rogue Core root {}", root.display()),
        source,
    })?;
    if !canonical_root.is_dir() {
        return Err(RogueCoreError::UnsupportedLayout(
            "Rogue Core root must be a directory".to_owned(),
        ));
    }
    let variant_descriptor = descriptor()
        .variants
        .into_iter()
        .find(|candidate| candidate.variant_id == variant.id())
        .expect("descriptor contains every RogueCoreVariant");
    let main_pak = canonical_root.join(&variant_descriptor.main_pak_relative_path);
    let metadata = fs::symlink_metadata(&main_pak).map_err(|source| RogueCoreError::Io {
        scope: format!("inspecting Rogue Core main PAK {}", main_pak.display()),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RogueCoreError::UnsupportedLayout(format!(
            "{} is not a regular Rogue Core main PAK",
            main_pak.display()
        )));
    }
    for relative in [
        &variant_descriptor.paks_relative_path,
        &variant_descriptor.binaries_relative_path,
    ] {
        if !canonical_root.join(relative).is_dir() {
            return Err(RogueCoreError::UnsupportedLayout(format!(
                "required Rogue Core directory is absent: {relative}"
            )));
        }
    }

    validate_supported_main_pak(&main_pak)?;
    Ok(ValidatedGameInstallation {
        game_id: ROGUE_CORE_GAME_ID.to_owned(),
        variant_id: variant.id().to_owned(),
        root_path: canonical_root.to_string_lossy().into_owned(),
        main_pak_relative_path: variant_descriptor.main_pak_relative_path,
        fingerprint: hash_file(&main_pak)?,
        target: variant_descriptor.target,
        main_pak_size: metadata.len(),
    })
}

pub(crate) fn validate_supported_main_pak(path: &Path) -> Result<(), RogueCoreError> {
    let mut reader = File::open(path).map_err(|source| RogueCoreError::Io {
        scope: "opening Rogue Core main PAK".to_owned(),
        source,
    })?;
    let archive = PakArchive::open(&mut reader)?;
    if archive.metadata().encrypted_index {
        return Err(RogueCoreError::UnsupportedVersion(
            "encrypted Rogue Core PAK indexes are unsupported".to_owned(),
        ));
    }
    if archive.metadata().version != "V11" {
        return Err(RogueCoreError::UnsupportedVersion(format!(
            "Rogue Core adapter supports PAK V11, found {}",
            archive.metadata().version
        )));
    }
    let stored = archive
        .metadata()
        .files
        .iter()
        .find(|stored| {
            normalize_entry_path(&archive.metadata().mount_point, stored).is_ok_and(|normalized| {
                normalized.eq_ignore_ascii_case("RogueCore/AssetRegistry.bin")
            })
        })
        .cloned()
        .ok_or_else(|| {
            RogueCoreError::UnsupportedVersion(
                "Rogue Core main PAK lacks RogueCore/AssetRegistry.bin".to_owned(),
            )
        })?;
    let mut bytes = Vec::new();
    archive
        .read_file_to(&stored, &mut reader, &mut bytes)
        .map_err(|error| {
            RogueCoreError::UnsupportedVersion(format!(
                "Rogue Core AssetRegistry could not be read: {error}"
            ))
        })?;
    AssetRegistry::read(&mut Cursor::new(bytes)).map_err(|error| {
        RogueCoreError::UnsupportedVersion(format!(
            "Rogue Core AssetRegistry format is unsupported: {error:#}"
        ))
    })?;
    Ok(())
}

pub fn validate_main_pak_path(
    path: &Path,
    variant: RogueCoreVariant,
) -> Result<PathBuf, RogueCoreError> {
    if path.file_name().and_then(|name| name.to_str()) != Some("RogueCore-Windows.pak") {
        return Err(RogueCoreError::UnsupportedLayout(format!(
            "main PAK path must end with {MAIN_PAK}"
        )));
    }
    let mut root = path;
    for _ in 0..Path::new(MAIN_PAK).components().count() {
        root = root.parent().ok_or_else(|| {
            RogueCoreError::UnsupportedLayout(format!(
                "main PAK is not beneath the expected {MAIN_PAK} layout"
            ))
        })?;
    }
    if root.join(MAIN_PAK) != path {
        return Err(RogueCoreError::UnsupportedLayout(format!(
            "main PAK path must end with {MAIN_PAK}"
        )));
    }
    RogueCoreVariant::from_id(variant.id())?;
    Ok(root.to_path_buf())
}

pub fn ensure_not_running(
    variant: RogueCoreVariant,
    processes: &[RunningProcess],
) -> Result<(), RogueCoreError> {
    let expected = descriptor()
        .variants
        .into_iter()
        .find(|candidate| candidate.variant_id == variant.id())
        .expect("descriptor contains every RogueCoreVariant")
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
        Err(RogueCoreError::GameRunning(matches.join(", ")))
    }
}

fn hash_file(path: &Path) -> Result<String, RogueCoreError> {
    let mut file = File::open(path).map_err(|source| RogueCoreError::Io {
        scope: format!("opening {} for fingerprint", path.display()),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| RogueCoreError::Io {
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
