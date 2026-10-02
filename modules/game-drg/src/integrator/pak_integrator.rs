use mintcat_integrator_core::common::audio_pak::{
    audio_pak_filename, verify_audio_only_from_bytes,
    verify_audio_only_pak_file, zip_contains_file_name,
};
use mintcat_integrator_core::common::mod_bundle_writer::ModBundleWriter;
use mintcat_integrator_core::common::ue4ss::{
    dir_contains_js_mod, install_ue4ss, install_ue4ss_js_mod_from_dir,
    install_ue4ss_js_mod_from_zip_targeted, install_ue4ss_mod,
    zip_contains_js_mod,
};
use mintcat_integrator_core::common::unpacked_mod::UnpackedMod;
use mintcat_integrator_core::common::zip::read_files_from_zip_by_extension;
use crate::integrator::game_pak_patch;
use crate::integrator::game_pak_patch::{
    get_deferred_paths, ESCAPE_MENU_PATH, MODDING_TAB_PATH, PATCH_PATHS, PCB_PATH,
    SERVER_LIST_ENTRY_PATH,
};
use crate::integrator::installation::DRGInstallation;
use crate::integrator::raw_asset::RawAsset;
use mintcat_integrator_core::progress::{json_value, text, InstallEvent, InstallProgress};
use mintcat_integrator_core::uasset_utils::asset_registry::{AssetRegistry, Readable as _, Writable as _};
use mintcat_integrator_core::uasset_utils::paths::PakPath;
use mintcat_integrator_core::{ModInfo, ReadSeek};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, BufWriter, Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use unreal_asset::engine_version::EngineVersion;
use unreal_asset::AssetBuilder;
use zip::read::ZipArchive;

type ModFileReaders = (Option<Box<dyn ReadSeek>>, Option<Box<dyn ReadSeek>>);

pub struct PakIntegrator {
    installation: DRGInstallation,
    asset_registry: AssetRegistry,
    bundle: ModBundleWriter<BufWriter<fs::File>>,
    deferred_assets: HashMap<&'static str, RawAsset>, // 延迟加载并且等待被 Patch 的资产
    added_paths: HashSet<String>,                     // 防止重复写入
    init_space_rig_assets: HashSet<String>,
    init_cave_assets: HashSet<String>,
}

impl PakIntegrator {
    fn format_soft_class(&self, path: &Path) -> String {
        let prefix = self.installation.content_prefix_for_path();
        let name = path.file_stem().unwrap().to_string_lossy();
        let relative = path
            .strip_prefix(prefix)
            .unwrap_or(path)
            .to_string_lossy()
            .strip_suffix("uasset")
            .unwrap_or(&*path.to_string_lossy())
            .to_string();
        format!("/Game/{}{}_C", relative, name)
    }
    pub fn new<P: AsRef<Path>>(fsd_path_pak: P, compress_mod_pak: bool) -> Result<Self> {
        let pak_path = fsd_path_pak.as_ref();
        let installation = DRGInstallation::from_pak_path(pak_path)
            .context("Failed to determine game installation")?;
        let ar_path = installation.asset_registry_pak_path();

        let mut fsd_pak_reader = BufReader::new(
            fs::File::open(pak_path)
                .with_context(|| format!("Failed to open game pak: {:?}", pak_path))?,
        );
        let fsd_pak = repak::PakBuilder::new()
            .reader(&mut fsd_pak_reader)
            .context("Failed to parse game pak")?;

        let asset_registry = AssetRegistry::read(&mut Cursor::new(
            fsd_pak.get(ar_path, &mut fsd_pak_reader).with_context(|| {
                format!(
                    "Failed to read AssetRegistry.bin from game pak (path: {})",
                    ar_path
                )
            })?,
        ))
        .context("Failed to parse AssetRegistry")?;

        let mut deferred_assets = Self::init_deferred_assets();
        Self::load_deferred_assets_for_game_pak(
            &fsd_pak,
            &mut fsd_pak_reader,
            &mut deferred_assets,
        )?;

        let mod_pak_path = installation.paks_path().join(installation.mod_pak_name());
        let bundle = ModBundleWriter::new(
            BufWriter::new(
                fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&mod_pak_path)
                    .with_context(|| format!("Failed to create mod pak: {:?}", mod_pak_path))?,
            ),
            &fsd_pak.files(),
            compress_mod_pak,
        )
        .context("Failed to initialize mod bundle writer")?;

        Ok(Self {
            installation,
            asset_registry,
            deferred_assets,
            bundle,
            added_paths: HashSet::new(),
            init_space_rig_assets: HashSet::new(),
            init_cave_assets: HashSet::new(),
        })
    }

    fn init_deferred_assets() -> HashMap<&'static str, RawAsset> {
        let deferred_paths = get_deferred_paths();
        HashMap::from_iter(
            deferred_paths
                .iter()
                .map(|&path| (path, RawAsset::default())),
        )
    }

    fn load_deferred_assets_for_game_pak(
        pak: &repak::PakReader,
        reader: &mut (impl Read + Seek),
        assets: &mut HashMap<&str, RawAsset>,
    ) -> Result<()> {
        for (path, asset) in assets.iter_mut() {
            asset.uasset = match pak.get(&format!("{path}.uasset"), reader) {
                Ok(f) => Some(f),
                Err(repak::Error::MissingEntry(_)) => None,
                Err(e) => return Err(e).with_context(|| format!("Failed to read {}.uasset", path)),
            };
            asset.uexp = match pak.get(&format!("{path}.uexp"), reader) {
                Ok(f) => Some(f),
                Err(repak::Error::MissingEntry(_)) => None,
                Err(e) => return Err(e).with_context(|| format!("Failed to read {}.uexp", path)),
            };
        }
        Ok(())
    }

    pub fn install(
        mut self,
        progress: &dyn InstallProgress,
        mods: &mut [ModInfo],
        skip_ue4ss: bool,
        ue4ss_zip_path: Option<&Path>,
        drg_zip_path: Option<&Path>,
    ) -> Result<()> {
        let total_percent = 70.0;
        let mods_size = mods.len();
        let ue4ss_enabled = !skip_ue4ss;

        // Install UE4SS once before processing mods (unless skipped)
        if ue4ss_enabled {
            progress.emit(InstallEvent::StatusLog(text("backend.install.ue4ss")))?;
            install_ue4ss(&self.installation.binaries_directory(), ue4ss_zip_path)?;
        }

        for (current_index, mod_info) in mods.iter_mut().enumerate() {
            progress.emit(InstallEvent::StatusLog(json_value(
                json!({ "key": "backend.install.process_mod_start", "name": mod_info.name }),
            )))?;

            let current_percent = (current_index as f32 / mods_size as f32) * total_percent + 10.0;
            progress.emit(InstallEvent::Percent(current_percent))?;

            let result = self.process_mod(mod_info, ue4ss_enabled);
            match result {
                Ok(_) => {
                    progress.emit(InstallEvent::StatusLog(json_value(json!({ "key": "backend.install.process_mod_success", "name": mod_info.name }))))?;
                }
                Err(_) => {
                    progress.emit(InstallEvent::Error(json_value(
                        json!({ "key": "backend.install.mod_failed", "name": mod_info.name }),
                    )))?;
                    return Err(anyhow::anyhow!("Mod install failed: {}", mod_info.name));
                }
            }
        }

        if drg_zip_path.is_some() {
            progress.emit(InstallEvent::StatusLog(text("backend.install.patch_pak")))?;
            progress.emit(InstallEvent::Percent(80.0))?;

            let drg_zip = drg_zip_path
                .ok_or_else(|| anyhow::anyhow!("DRG asset zip path is required (DRG.zip)"))?;
            let mut mint_files = HashMap::new();
            Self::collect_mint_files_from_drg_zip(drg_zip, &mut mint_files)?;
            self.process_loose_asset_files(&mint_files)?;
            Self::write_hook_dll_from_drg_zip(drg_zip, &self.installation.binaries_directory())?;

            self.apply_mint_patch(&mut mint_files)?;
            self.apply_pcb_patch()?;
            self.apply_sandbox_patch()?;

            progress.emit(InstallEvent::StatusLog(text("backend.install.write_mod")))?;
            progress.emit(InstallEvent::Percent(90.0))?;

            self.write_mint_files(&mut mint_files)?;
        } else {
            progress.emit(InstallEvent::StatusLog(text("backend.install.write_mod")))?;
            progress.emit(InstallEvent::Percent(90.0))?;
        }

        self.serialize_asset_registry()?;
        //ensure_ue4ss_config_directory(&binaries_dir)?;
        //self.write_ue4ss_mods_config()?;
        self.bundle.finish().context("Failed to finalize mod pak")?;

        progress.emit(InstallEvent::StatusLog(text("backend.install.success")))?;
        progress.emit(InstallEvent::Percent(100.0))?;

        let mod_pak_path = self
            .installation
            .paks_path()
            .join(self.installation.mod_pak_name());

        //recovery_modio(&self.installation).unwrap();

        let metadata =
            fs::metadata(&mod_pak_path).context("Failed to get mod pak metadata after install")?;
        let mod_pak_timestamp = metadata
            .modified()
            .context("Failed to get mod pak modified time")?
            .duration_since(std::time::UNIX_EPOCH)
            .context("Failed to calculate timestamp")?
            .as_secs();

        progress.emit(InstallEvent::Success(mod_pak_timestamp))?;

        Ok(())
    }

    /// Direct-copy an audio-only mod pak to the game Paks directory, bypassing the full
    /// repackaging pipeline. Returns `Ok(true)` if direct copy succeeded, `Ok(false)` if
    /// verification failed and the mod should fall back to normal processing.
    fn game_pak_stem(&self) -> &str {
        self.installation
            .pak_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("FSD-WindowsNoEditor")
    }

    fn try_direct_copy_audio_mod(&self, mod_info: &ModInfo) -> Result<bool> {
        let pak_path = Path::new(&mod_info.pak_path);
        let target_path = self
            .installation
            .paks_path()
            .join(audio_pak_filename(self.game_pak_stem(), &mod_info.name));

        let mut header_buf = [0u8; 4];
        let mut file = fs::File::open(pak_path)
            .with_context(|| format!("Failed to open mod file: {:?}", pak_path))?;
        file.read_exact(&mut header_buf)
            .with_context(|| format!("Failed to read mod file header: {:?}", pak_path))?;
        drop(file);

        if header_buf == [0x50, 0x4B, 0x03, 0x04] {
            if let Ok(dlls) = read_files_from_zip_by_extension(pak_path.to_str().unwrap(), "dll") {
                if !dlls.is_empty() {
                    return Ok(false);
                }
            }
            if zip_contains_js_mod(pak_path) {
                return Ok(false);
            }
            if zip_contains_file_name(pak_path, "AssetRegistry.bin")? {
                return Ok(false);
            }
            if let Ok(paks) = read_files_from_zip_by_extension(pak_path.to_str().unwrap(), "pak") {
                if let Some((_, pak_data)) = paks.first() {
                    if !verify_audio_only_from_bytes(pak_data)? {
                        return Ok(false);
                    }
                    fs::write(&target_path, pak_data)
                        .with_context(|| format!("Failed to write audio pak: {:?}", target_path))?;
                    return Ok(true);
                }
            }
            Ok(false)
        } else {
            if !verify_audio_only_pak_file(pak_path)? {
                return Ok(false);
            }
            fs::copy(pak_path, &target_path)
                .with_context(|| format!("Failed to copy audio pak to: {:?}", target_path))?;
            Ok(true)
        }
    }

    fn process_mod(&mut self, mod_info: &mut ModInfo, ue4ss_enabled: bool) -> Result<()> {
        let pak_path_str = mod_info.pak_path.clone();
        let pak_path = Path::new(&pak_path_str);

        if mod_info.is_unpacked {
            return self
                .process_directory_mod(mod_info, pak_path, ue4ss_enabled)
                .with_context(|| format!("Failed to process directory mod: {}", mod_info.name));
        }

        if mod_info.is_audio_only {
            match self.try_direct_copy_audio_mod(mod_info) {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    log::info!(
                        "Audio-only verification failed for '{}', falling back to normal processing",
                        mod_info.name
                    );
                }
                Err(e) => {
                    log::warn!(
                        "Audio direct copy failed for '{}', falling back: {:#}",
                        mod_info.name,
                        e
                    );
                }
            }
        }

        let (mut pak_buf, mut dll_buf) = self
            .load_mod_files(pak_path)
            .with_context(|| format!("Failed to load mod files: {:?}", pak_path))?;
        if let Some(ref mut pak) = pak_buf {
            self.process_pak_files(pak)
                .with_context(|| format!("Failed to process pak for mod: {}", mod_info.name))?;
        }
        if ue4ss_enabled {
            if let Some(ref mut dll) = dll_buf {
                self.process_dll_files(mod_info, dll)
                    .with_context(|| format!("Failed to process dll for mod: {}", mod_info.name))?;
            }
        }
        if ue4ss_enabled && zip_contains_js_mod(pak_path) {
            install_ue4ss_js_mod_from_zip_targeted(
                &self.installation.binaries_directory(),
                &mod_info.name,
                pak_path,
            )
            .with_context(|| format!("Failed to install JS mod: {}", mod_info.name))?;
        }
        Ok(())
    }

    /// Process a directory-based mod that may contain multiple content types:
    /// Content/ (unpacked UE assets), pak/ (.pak files), js/ (UE4SS scripts), dll/ (UE4SS native mods)
    fn process_directory_mod(
        &mut self,
        mod_info: &mut ModInfo,
        path: &Path,
        ue4ss_enabled: bool,
    ) -> Result<()> {
        let mut processed_any = false;

        if UnpackedMod::is_valid_unpacked_mod(path) {
            self.process_unpacked_mod(path).with_context(|| {
                format!("Failed to process unpacked content: {}", mod_info.name)
            })?;
            processed_any = true;
        }

        let pak_dir = path.join("pak");
        if pak_dir.is_dir() {
            for entry in fs::read_dir(&pak_dir)
                .with_context(|| format!("Failed to read pak directory: {:?}", pak_dir))?
            {
                let entry = entry?;
                let entry_path = entry.path();
                if entry_path.is_file()
                    && entry_path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("pak"))
                {
                    let file = fs::File::open(&entry_path)
                        .with_context(|| format!("Failed to open pak: {:?}", entry_path))?;
                    let mut reader: Box<dyn ReadSeek> = Box::new(BufReader::new(file));
                    self.process_pak_files(&mut reader)
                        .with_context(|| format!("Failed to process pak: {:?}", entry_path))?;
                    processed_any = true;
                }
            }
        }

        if dir_contains_js_mod(path) {
            if ue4ss_enabled {
                install_ue4ss_js_mod_from_dir(
                    &self.installation.binaries_directory(),
                    &mod_info.name,
                    path,
                )
                .with_context(|| format!("Failed to install JS mod: {}", mod_info.name))?;
            }
            processed_any = true;
        }

        let dll_dir = path.join("dll");
        if dll_dir.is_dir() {
            if !ue4ss_enabled {
                return Ok(());
            }
            for entry in fs::read_dir(&dll_dir)
                .with_context(|| format!("Failed to read dll directory: {:?}", dll_dir))?
            {
                let entry = entry?;
                let entry_path = entry.path();
                if entry_path.is_file()
                    && entry_path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("dll"))
                {
                    let file = fs::File::open(&entry_path)
                        .with_context(|| format!("Failed to open dll: {:?}", entry_path))?;
                    let mut reader: Box<dyn ReadSeek> = Box::new(BufReader::new(file));
                    install_ue4ss_mod(
                        &self.installation.binaries_directory(),
                        &mod_info.name,
                        &mut reader,
                    )?;
                    processed_any = true;
                    break;
                }
            }
        }

        if !processed_any {
            anyhow::bail!("No recognized mod content in directory: {:?}", path);
        }

        Ok(())
    }

    /// Process an unpacked mod directory (contains Content folder with uasset/uexp files)
    fn process_unpacked_mod(&mut self, mod_path: &Path) -> Result<()> {
        let mut unpacked_mod = UnpackedMod::new(mod_path)
            .with_context(|| format!("Failed to create unpacked mod: {:?}", mod_path))?;

        unpacked_mod
            .load_files(self.installation.content_prefix())
            .with_context(|| format!("Failed to load unpacked mod files: {:?}", mod_path))?;

        let files = unpacked_mod.files();
        self.process_loose_asset_files(files)?;

        // Write all files to the bundle
        for (pak_path, data) in unpacked_mod.iter() {
            let lowercase = pak_path.to_lowercase();
            if self.added_paths.contains(&lowercase) {
                continue;
            }

            // Skip AssetRegistry.bin and shader bytecode files
            if pak_path.ends_with("AssetRegistry.bin") || pak_path.ends_with(".ushaderbytecode") {
                continue;
            }

            self.bundle
                .write_file(data, pak_path)
                .with_context(|| format!("Failed to write unpacked mod file: {}", pak_path))?;

            self.added_paths.insert(lowercase);
        }

        Ok(())
    }

    fn load_mod_files(&self, path: &Path) -> Result<ModFileReaders> {
        let mut buf = [0; 4];
        let mut file =
            fs::File::open(path).with_context(|| format!("Failed to open mod file: {:?}", path))?;
        file.read_exact(&mut buf)
            .with_context(|| format!("Failed to read mod file header: {:?}", path))?;

        if buf == [0x50, 0x4B, 0x03, 0x04] {
            // ZIP file
            let mut pak: Option<Box<dyn ReadSeek>> = None;
            let mut dll: Option<Box<dyn ReadSeek>> = None;

            if let Ok(paks) = read_files_from_zip_by_extension(path.to_str().unwrap(), "pak") {
                if let Some((_, data)) = paks.first() {
                    pak = Some(Box::new(Cursor::new(data.clone())));
                }
            }

            if let Ok(dlls) = read_files_from_zip_by_extension(path.to_str().unwrap(), "dll") {
                if let Some((_, data)) = dlls.first() {
                    dll = Some(Box::new(Cursor::new(data.clone())));
                }
            }

            Ok((pak, dll))
        } else {
            // PAK file (not ZIP)
            Ok((Some(Box::new(BufReader::new(file))), None))
        }
    }

    fn process_pak_files(&mut self, pak_buf: &mut Box<dyn ReadSeek>) -> Result<()> {
        let pak = repak::PakBuilder::new()
            .reader(pak_buf)
            .context("Failed to parse mod pak")?;

        let mount = PakPath::new(pak.mount_point());
        let pak_files = self.normalize_pak_paths(&pak, mount)?;

        self.process_init_asset(&pak_files)?;
        self.process_asset_registry(&pak, &pak_files, pak_buf)?;
        self.write_mod_assets(pak, pak_files, pak_buf)?;
        Ok(())
    }

    fn process_init_asset(&mut self, pak_files: &HashMap<PathBuf, String>) -> Result<()> {
        for pak_file in pak_files {
            if let Some(filename) = pak_file.0.file_name() {
                let lower = filename.to_string_lossy().to_lowercase();
                if lower == "initspacerig.uasset" {
                    self.init_space_rig_assets
                        .insert(self.format_soft_class(pak_file.0));
                }
                if lower == "initcave.uasset" {
                    self.init_cave_assets
                        .insert(self.format_soft_class(pak_file.0));
                }
            }
        }
        Ok(())
    }

    fn process_dll_files(
        &mut self,
        mod_info: &mut ModInfo,
        dll_buf: &mut Box<dyn ReadSeek>,
    ) -> Result<()> {
        install_ue4ss_mod(
            &self.installation.binaries_directory(),
            &mod_info.name,
            dll_buf,
        )?;
        Ok(())
    }

    fn normalize_pak_paths(
        &self,
        pak: &repak::PakReader,
        mount: &PakPath,
    ) -> Result<HashMap<PathBuf, String>> {
        pak.files()
            .into_iter()
            .map(|p| {
                let full_path = mount.join(&p);
                let normalized = full_path
                    .strip_prefix("../../../")
                    .with_context(|| format!("Invalid pak path: {}", p))?;
                let std_path = PathBuf::from(normalized.as_str());
                Ok((std_path, p))
            })
            .collect()
    }

    fn process_asset_registry(
        &mut self,
        pak: &repak::PakReader,
        pak_files: &HashMap<PathBuf, String>,
        pak_buf: &mut Box<dyn ReadSeek>,
    ) -> Result<()> {
        for (normalized, pak_path) in pak_files {
            if let Some("uasset" | "umap") = normalized.extension().and_then(|e| e.to_str()) {
                if pak_files.contains_key(&normalized.with_extension("uexp")) {
                    let uasset = pak
                        .get(pak_path, pak_buf)
                        .with_context(|| format!("Failed to read uasset: {}", pak_path))?;
                    let uexp_path = PakPath::new(pak_path).with_extension("uexp");
                    let uexp = pak
                        .get(uexp_path.as_str(), pak_buf)
                        .with_context(|| format!("Failed to read uexp for: {}", pak_path))?;
                    let asset = AssetBuilder::new(Cursor::new(uasset), EngineVersion::VER_UE4_27)
                        .bulk(Cursor::new(uexp))
                        .skip_data(true)
                        .build()
                        .with_context(|| format!("Failed to build asset: {}", pak_path))?;

                    self.asset_registry
                        .populate(normalized.with_extension("").to_str().unwrap(), &asset)
                        .with_context(|| {
                            format!("Failed to populate asset registry for: {:?}", normalized)
                        })?;
                }
            }
        }
        Ok(())
    }

    fn process_asset_registry_from_files(
        &mut self,
        files: &HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        for (path, uasset) in files {
            let normalized = PathBuf::from(path);
            if let Some("uasset" | "umap") = normalized.extension().and_then(|e| e.to_str()) {
                let uexp_path = normalized.with_extension("uexp");
                let Some(uexp) = files.get(uexp_path.to_str().unwrap()) else {
                    continue;
                };

                let asset =
                    AssetBuilder::new(Cursor::new(uasset.as_slice()), EngineVersion::VER_UE4_27)
                        .bulk(Cursor::new(uexp.as_slice()))
                        .skip_data(true)
                        .build()
                        .with_context(|| format!("Failed to build asset: {}", path))?;

                self.asset_registry
                    .populate(normalized.with_extension("").to_str().unwrap(), &asset)
                    .with_context(|| {
                        format!("Failed to populate asset registry for: {:?}", normalized)
                    })?;
            }
        }
        Ok(())
    }

    fn process_loose_asset_files(&mut self, files: &HashMap<String, Vec<u8>>) -> Result<()> {
        let pak_files: HashMap<PathBuf, String> = files
            .keys()
            .map(|path| (PathBuf::from(path), path.clone()))
            .collect();

        self.process_init_asset(&pak_files)?;
        self.process_asset_registry_from_files(files)?;
        Ok(())
    }

    fn write_mod_assets(
        &mut self,
        pak: repak::PakReader,
        pak_files: HashMap<PathBuf, String>,
        pak_buf: &mut Box<dyn ReadSeek>,
    ) -> Result<()> {
        for (pak_file, pak_path) in pak_files {
            let lowercase = pak_file.to_str().unwrap().to_lowercase();
            if self.added_paths.contains(&lowercase) {
                continue;
            }

            if let Some(filename) = pak_file.file_name() {
                if filename == "AssetRegistry.bin" {
                    continue;
                }
                if pak_file.extension().and_then(std::ffi::OsStr::to_str) == Some("ushaderbytecode")
                {
                    continue;
                }
            }

            let file_data = pak
                .get(&pak_path, pak_buf)
                .with_context(|| format!("Failed to read file from pak: {}", pak_path))?;
            self.bundle
                .write_file(&file_data, pak_file.to_str().unwrap())
                .with_context(|| format!("Failed to write mod file: {:?}", pak_file))?;

            self.added_paths.insert(lowercase);
        }
        Ok(())
    }

    fn apply_mint_patch(&mut self, mint_files: &mut HashMap<String, Vec<u8>>) -> Result<()> {
        let mint_path = (
            "FSD/Content/_AssemblyStorm/ModIntegration/MI_SpawnMods.uasset",
            "FSD/Content/_AssemblyStorm/ModIntegration/MI_SpawnMods.uexp",
        );

        let mut asset = unreal_asset::Asset::new(
            Cursor::new(mint_files[mint_path.0].clone()),
            Some(Cursor::new(mint_files[mint_path.1].clone())),
            EngineVersion::VER_UE4_27,
            None,
            false,
        )
        .context("Failed to parse MI_SpawnMods asset")?;

        game_pak_patch::patch_init_actors(
            &mut asset,
            self.init_space_rig_assets.clone(),
            self.init_cave_assets.clone(),
        );
        self.bundle
            .write_asset(
                asset,
                "FSD/Content/_AssemblyStorm/ModIntegration/MI_SpawnMods",
            )
            .context("Failed to write MI_SpawnMods asset")?;

        Ok(())
    }

    fn apply_pcb_patch(&mut self) -> Result<()> {
        let mut asset = self.deferred_assets[PCB_PATH]
            .parse()
            .context("Failed to parse PCB asset")?;
        game_pak_patch::hook_pcb(&mut asset).context("Failed to apply PCB hook")?;
        self.bundle
            .write_asset(asset, PCB_PATH)
            .context("Failed to write PCB asset")?;
        Ok(())
    }

    fn apply_sandbox_patch(&mut self) -> Result<()> {
        for path in PATCH_PATHS.iter() {
            let mut asset = self.deferred_assets[path]
                .parse()
                .with_context(|| format!("Failed to parse sandbox asset: {}", path))?;
            game_pak_patch::patch_sandbox(&mut asset)
                .with_context(|| format!("Failed to apply sandbox patch: {}", path))?;
            self.bundle
                .write_asset(asset, path)
                .with_context(|| format!("Failed to write sandbox asset: {}", path))?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    fn apply_escape_menu_patch(&mut self) -> Result<()> {
        let mut asset = self.deferred_assets[ESCAPE_MENU_PATH]
            .parse()
            .context("Failed to parse escape menu asset")?;
        game_pak_patch::patch_modding_tab(&mut asset)
            .context("Failed to apply escape menu patch")?;
        self.bundle
            .write_asset(asset, ESCAPE_MENU_PATH)
            .context("Failed to write escape menu asset")?;
        Ok(())
    }

    #[allow(dead_code)]
    fn apply_modding_tab_patch(&mut self) -> Result<()> {
        let mut asset = self.deferred_assets[MODDING_TAB_PATH]
            .parse()
            .context("Failed to parse modding tab asset")?;
        game_pak_patch::patch_modding_tab_item(&mut asset)
            .context("Failed to apply modding tab patch")?;
        self.bundle
            .write_asset(asset, MODDING_TAB_PATH)
            .context("Failed to write modding tab asset")?;
        Ok(())
    }

    #[allow(dead_code)]
    fn apply_server_list_entry_patch(&mut self) -> Result<()> {
        let mut asset = self.deferred_assets[SERVER_LIST_ENTRY_PATH]
            .parse()
            .context("Failed to parse server list entry asset")?;
        game_pak_patch::patch_server_list_entry(&mut asset)
            .context("Failed to apply server list entry patch")?;
        self.bundle
            .write_asset(asset, SERVER_LIST_ENTRY_PATH)
            .context("Failed to write server list entry asset")?;
        Ok(())
    }

    fn write_mint_files(&mut self, mint_files: &mut HashMap<String, Vec<u8>>) -> Result<()> {
        let mint_path = (
            "FSD/Content/_AssemblyStorm/ModIntegration/MI_SpawnMods.uasset",
            "FSD/Content/_AssemblyStorm/ModIntegration/MI_SpawnMods.uexp",
        );
        mint_files.remove(mint_path.0);
        mint_files.remove(mint_path.1);
        for (path, data) in mint_files {
            self.bundle
                .write_file(data.as_slice(), path.as_str())
                .with_context(|| format!("Failed to write mint file: {}", path))?;
        }
        Ok(())
    }

    /// 从 DRG.zip 读取 Paks/ 目录下全部文件，映射为 FSD/Content/ 下的 pak 路径。
    fn collect_mint_files_from_drg_zip(
        zip_path: &Path,
        files: &mut HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        const ZIP_PREFIX: &str = "Paks/";
        const KEY_PREFIX: &str = "FSD/Content/";
        let file = fs::File::open(zip_path)
            .with_context(|| format!("Failed to open DRG zip: {:?}", zip_path))?;
        let mut archive = ZipArchive::new(file).context("Failed to parse DRG zip")?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).context("Failed to read zip entry")?;
            let name = entry.name().to_string();
            let name_normalized = name.replace('\\', "/");
            if !name_normalized.starts_with(ZIP_PREFIX) || name_normalized.ends_with('/') {
                continue;
            }
            let suffix = name_normalized.trim_start_matches(ZIP_PREFIX);
            let key = format!("{KEY_PREFIX}{}", suffix);
            let mut content = Vec::new();
            entry
                .read_to_end(&mut content)
                .context("Failed to read zip entry content")?;
            files.insert(key, content);
        }
        Ok(())
    }

    /// Extract DLLs/x3daudio1_7.dll from DRG.zip to binaries directory.
    fn write_hook_dll_from_drg_zip(zip_path: &Path, binaries_dir: &Path) -> Result<()> {
        const HOOK_ENTRY: &str = "DLLs/x3daudio1_7.dll";
        let file = fs::File::open(zip_path)
            .with_context(|| format!("Failed to open DRG zip: {:?}", zip_path))?;
        let mut archive = ZipArchive::new(file).context("Failed to parse DRG zip")?;
        let mut entry = archive
            .by_name(HOOK_ENTRY)
            .with_context(|| format!("Missing {} in DRG zip", HOOK_ENTRY))?;
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .context("Failed to read hook dll from zip")?;
        let hook_path = binaries_dir.join("x3daudio1_7.dll");
        fs::write(&hook_path, content)
            .with_context(|| format!("Failed to write hook dll: {:?}", hook_path))?;
        Ok(())
    }

    fn serialize_asset_registry(&mut self) -> Result<()> {
        let mut buf = Vec::new();
        self.asset_registry
            .write(&mut buf)
            .context("Failed to serialize asset registry")?;
        self.bundle
            .write_file(&buf, self.installation.asset_registry_pak_path())
            .context("Failed to write asset registry to mod pak")?;
        Ok(())
    }

}
