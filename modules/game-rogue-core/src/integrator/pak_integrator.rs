//! Rogue Core (UE 5.6) mod 整合：仅合并 mod pak / unpacked，不做 FSD 专用 patch

use mintcat_integrator_core::common::audio_pak::{
    audio_pak_filename, verify_audio_only_from_bytes,
    verify_audio_only_pak_file, zip_contains_file_name,
};
use mintcat_integrator_core::common::mod_bundle_writer::ModBundleWriter;
use mintcat_integrator_core::common::ue4ss::{
    dir_contains_js_mod, install_ue4ss, install_ue4ss_js_mod_from_dir,
    install_ue4ss_js_mod_from_zip_targeted, install_ue4ss_mod, zip_contains_js_mod,
};
use mintcat_integrator_core::common::unpacked_mod::UnpackedMod;
use mintcat_integrator_core::common::zip::read_files_from_zip_by_extension;
use mintcat_integrator_core::progress::{json_value, text, InstallEvent, InstallProgress};
use mintcat_integrator_core::uasset_utils::asset_registry::{
    AssetRegistry, Dependencies, Names, Readable as _, Store,
};
use mintcat_integrator_core::uasset_utils::paths::PakPath;
use mintcat_integrator_core::{ModInfo, ReadSeek};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, BufWriter, Cursor, Read};
use std::path::{Path, PathBuf};
use unreal_asset::engine_version::EngineVersion;
use unreal_asset::AssetBuilder;
use zip::read::ZipArchive;

use crate::integrator::{
    installation::RcInstallation, ue4ss::ensure_rogue_core_ue4ss_settings_file,
};

type ModFileReaders = (Option<Box<dyn ReadSeek>>, Option<Box<dyn ReadSeek>>);

/// 当游戏主 pak 使用 Oodle 压缩且运行时无法加载 Oodle（如网络超时）时，使用空 AssetRegistry 继续，
/// 仅合并 mod 内的资源到 registry。
fn empty_asset_registry() -> AssetRegistry {
    AssetRegistry {
        version: [0u8; 16],
        version_int: 0,
        hash_version: 0,
        names: Names(indexmap::IndexSet::new()),
        store: Store {
            pair_count: 0,
            texts: vec![],
            nbl_names: vec![],
            names: vec![],
            nbl_export_paths: vec![],
            export_paths: vec![],
            ansi_strings: vec![],
            wide_strings: vec![],
            pairs: vec![],
        },
        asset_data: vec![],
        dependencies: Dependencies {
            dependencies_size: 0,
            dependencies: vec![],
            package_data_buffer_size: 0,
        },
    }
}

fn is_oodle_or_network_error(e: &anyhow::Error) -> bool {
    let msg = format!("{:#}", e);
    msg.contains("Oodle")
        || msg.contains("Timeout")
        || msg.contains("ureq")
        || msg.contains("Global")
}

/// 解析失败但可回退到空 registry 的情况（如 UE5.6 与 uasset_utils 格式/编码差异）
fn is_asset_registry_parse_fallback(e: &anyhow::Error) -> bool {
    let msg = format!("{:#}", e);
    msg.contains("invalid utf-16")
        || msg.contains("lone surrogate")
        || msg.contains("Failed to parse AssetRegistry")
        || msg.contains("unexpected magic")
        || msg.contains("unsupported format")
}

pub struct RcPakIntegrator {
    installation: RcInstallation,
    asset_registry: AssetRegistry,
    bundle: ModBundleWriter<BufWriter<fs::File>>,
    added_paths: HashSet<String>,
}

impl RcPakIntegrator {
    pub fn new<P: AsRef<Path>>(game_pak_path: P, compress_mod_pak: bool) -> Result<Self> {
        let pak_path = game_pak_path.as_ref();
        let installation = RcInstallation::from_pak_path(pak_path)
            .context("Failed to determine RC installation")?;
        let ar_path = installation.asset_registry_pak_path();

        let mut reader = BufReader::new(
            fs::File::open(pak_path)
                .with_context(|| format!("Failed to open game pak: {:?}", pak_path))?,
        );
        let rc_pak = repak::PakBuilder::new()
            .reader(&mut reader)
            .context("Failed to parse RC game pak")?;

        let file_list: Vec<String> = rc_pak
            .files()
            .iter()
            .map(|p| p.as_str().to_string())
            .collect();

        let asset_registry = match rc_pak.get(ar_path, &mut reader) {
            Ok(ar_bytes) => match AssetRegistry::read(&mut Cursor::new(ar_bytes)) {
                Ok(ar) => ar,
                Err(e) => {
                    let err = e.context("Failed to parse AssetRegistry");
                    if is_asset_registry_parse_fallback(&err) {
                        log::warn!(
                            "RC game pak AssetRegistry parse failed (e.g. format/encoding): {:#}. Using empty registry and merging mod assets only.",
                            err
                        );
                        empty_asset_registry()
                    } else {
                        return Err(err);
                    }
                }
            },
            Err(e) => {
                let err =
                    anyhow::Error::from(e).context("Failed to read AssetRegistry from game pak");
                if is_oodle_or_network_error(&err) {
                    log::warn!(
                        "RC game pak AssetRegistry unavailable (Oodle/network): {:#}. Using empty registry and merging mod assets only.",
                        err
                    );
                    empty_asset_registry()
                } else {
                    return Err(err.context(format!("path: {}", ar_path)));
                }
            }
        };

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
            &file_list,
            compress_mod_pak,
        )
        .context("Failed to initialize mod bundle writer")?;

        Ok(Self {
            installation,
            asset_registry,
            bundle,
            added_paths: HashSet::new(),
        })
    }

    /// 从 RC.zip 读取 Paks/ 目录下全部文件，映射为 RogueCore/Content/ 下的 pak 路径。
    /// 跳过目录条目、__MACOSX、.DS_Store 等无关文件。
    fn collect_files_from_rc_zip(
        zip_path: &Path,
        files: &mut HashMap<String, Vec<u8>>,
    ) -> Result<()> {
        const ZIP_PREFIX: &str = "Paks/";
        const KEY_PREFIX: &str = "RogueCore/Content/";
        let file = fs::File::open(zip_path)
            .with_context(|| format!("Failed to open RC zip: {:?}", zip_path))?;
        let mut archive = ZipArchive::new(file).context("Failed to parse RC zip")?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).context("Failed to read zip entry")?;
            let name = entry.name().to_string();
            let name_normalized = name.replace('\\', "/");
            if !name_normalized.starts_with(ZIP_PREFIX) || name_normalized.ends_with('/') {
                continue;
            }
            if name_normalized.contains("__MACOSX") || name_normalized.contains(".DS_Store") {
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

    pub fn install(
        mut self,
        progress: &dyn InstallProgress,
        mods: &mut [ModInfo],
        skip_ue4ss: bool,
        ue4ss_zip_path: Option<&Path>,
        rc_zip_path: Option<&Path>,
    ) -> Result<()> {
        let total_percent = 70.0f32;
        let mods_size = mods.len();
        let ue4ss_enabled = !skip_ue4ss;

        // 有传入 ue4ssl.zip 且未勾选跳过时安装 UE4SS
        if ue4ss_enabled {
            if let Some(zip_path) = ue4ss_zip_path {
                progress.emit(InstallEvent::StatusLog(text("backend.install.ue4ss")))?;
                install_ue4ss(&self.installation.binaries_directory(), Some(zip_path))?;
            }
        }

        for (current_index, mod_info) in mods.iter_mut().enumerate() {
            progress.emit(InstallEvent::StatusLog(json_value(
                json!({ "key": "backend.install.process_mod_start", "name": mod_info.name }),
            )))?;
            let current_percent = (current_index as f32 / mods_size as f32) * total_percent + 10.0;
            progress.emit(InstallEvent::Percent(current_percent))?;

            if let Err(e) = self.process_mod(mod_info, ue4ss_enabled) {
                progress.emit(InstallEvent::Error(json_value(
                    json!({ "key": "backend.install.mod_failed", "name": mod_info.name }),
                )))?;
                return Err(e);
            }
            progress.emit(InstallEvent::StatusLog(json_value(
                json!({ "key": "backend.install.process_mod_success", "name": mod_info.name }),
            )))?;
        }

        progress.emit(InstallEvent::StatusLog(text("backend.install.write_mod")))?;
        progress.emit(InstallEvent::Percent(90.0))?;

        // RC.zip：将 Paks/ 目录下全部文件写入 mod pak（RogueCore/Content/...）
        if let Some(zip_path) = rc_zip_path {
            progress.emit(InstallEvent::StatusLog(text("backend.install.rc_zip")))?;
            let mut rc_files = HashMap::new();
            Self::collect_files_from_rc_zip(zip_path, &mut rc_files)?;
            for (pak_path, data) in rc_files {
                self.bundle
                    .write_file(&data, &pak_path)
                    .with_context(|| format!("Failed to write RC zip file: {}", pak_path))?;
            }
        }

        self.serialize_asset_registry()?;
        if ue4ss_enabled {
            ensure_rogue_core_ue4ss_settings_file(&self.installation.binaries_directory())?;
        }
        self.bundle.finish().context("Failed to finalize mod pak")?;

        progress.emit(InstallEvent::StatusLog(text("backend.install.success")))?;
        progress.emit(InstallEvent::Percent(100.0))?;

        let mod_pak_path = self
            .installation
            .paks_path()
            .join(self.installation.mod_pak_name());
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
            .unwrap_or("RogueCore-Windows")
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
        let path = Path::new(&pak_path_str);

        if mod_info.is_unpacked {
            return self
                .process_directory_mod(mod_info, path, ue4ss_enabled)
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
            .load_mod_files(path)
            .with_context(|| format!("Failed to load mod files: {:?}", path))?;
        if let Some(ref mut pak) = pak_buf {
            self.process_pak_files(pak)
                .with_context(|| format!("Failed to process pak for mod: {}", mod_info.name))?;
        }
        if ue4ss_enabled {
            if let Some(ref mut dll) = dll_buf {
                install_ue4ss_mod(&self.installation.binaries_directory(), &mod_info.name, dll)?;
            }
        }
        if ue4ss_enabled && zip_contains_js_mod(path) {
            install_ue4ss_js_mod_from_zip_targeted(
                &self.installation.binaries_directory(),
                &mod_info.name,
                path,
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

    fn process_unpacked_mod(&mut self, mod_path: &Path) -> Result<()> {
        let mut unpacked_mod = UnpackedMod::new(mod_path)
            .with_context(|| format!("Failed to create unpacked mod: {:?}", mod_path))?;
        unpacked_mod
            .load_files(self.installation.content_prefix())
            .with_context(|| format!("Failed to load unpacked mod files: {:?}", mod_path))?;

        for asset_base in unpacked_mod.get_asset_names() {
            if let Some((uasset_data, uexp_data)) = unpacked_mod.get_asset_pair(&asset_base) {
                let normalized_path = PathBuf::from(&asset_base);
                let asset =
                    AssetBuilder::new(Cursor::new(uasset_data.clone()), EngineVersion::VER_UE4_27)
                        .bulk(Cursor::new(uexp_data.clone()))
                        .skip_data(true)
                        .build()
                        .with_context(|| format!("Failed to build asset: {}", asset_base))?;
                self.asset_registry
                    .populate(normalized_path.to_str().unwrap(), &asset)
                    .with_context(|| {
                        format!(
                            "Failed to populate asset registry for: {:?}",
                            normalized_path
                        )
                    })?;
            }
        }

        for (pak_path, data) in unpacked_mod.iter() {
            let lowercase = pak_path.to_lowercase();
            if self.added_paths.contains(&lowercase) {
                continue;
            }
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
        let mut buf = [0u8; 4];
        let mut file =
            fs::File::open(path).with_context(|| format!("Failed to open mod file: {:?}", path))?;
        file.read_exact(&mut buf)
            .with_context(|| format!("Failed to read mod file header: {:?}", path))?;

        if buf == [0x50, 0x4B, 0x03, 0x04] {
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
            Ok((Some(Box::new(BufReader::new(file))), None))
        }
    }

    fn process_pak_files(&mut self, pak_buf: &mut Box<dyn ReadSeek>) -> Result<()> {
        let pak = repak::PakBuilder::new()
            .reader(pak_buf)
            .context("Failed to parse mod pak")?;
        let mount = PakPath::new(pak.mount_point());
        let pak_files = self.normalize_pak_paths(&pak, mount)?;
        self.process_asset_registry(&pak, &pak_files, pak_buf)?;
        self.write_mod_assets(pak, pak_files, pak_buf)?;
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
                Ok((PathBuf::from(normalized.as_str()), p))
            })
            .collect()
    }

    /// 从 mod pak 中的 uasset/uexp 填充 AssetRegistry。单个 asset 解析失败（如 UE5.6 格式差异）
    /// 时仅跳过并打 log，不中断整包；write_mod_assets 仍会按原始字节写入该文件。
    fn process_asset_registry(
        &mut self,
        pak: &repak::PakReader,
        pak_files: &HashMap<PathBuf, String>,
        pak_buf: &mut Box<dyn ReadSeek>,
    ) -> Result<()> {
        for (normalized, pak_path) in pak_files {
            if let Some("uasset" | "umap") = normalized.extension().and_then(|e| e.to_str()) {
                if !pak_files.contains_key(&normalized.with_extension("uexp")) {
                    continue;
                }
                let uasset = match pak.get(pak_path, pak_buf) {
                    Ok(d) => d,
                    Err(e) => {
                        log::warn!(
                            "Skip registry for {}: failed to read uasset: {}",
                            pak_path,
                            e
                        );
                        continue;
                    }
                };
                let uexp_path = PakPath::new(pak_path).with_extension("uexp");
                let uexp = match pak.get(uexp_path.as_str(), pak_buf) {
                    Ok(d) => d,
                    Err(e) => {
                        log::warn!("Skip registry for {}: failed to read uexp: {}", pak_path, e);
                        continue;
                    }
                };
                let asset = match AssetBuilder::new(Cursor::new(uasset), EngineVersion::VER_UE4_27)
                    .bulk(Cursor::new(uexp))
                    .skip_data(true)
                    .build()
                {
                    Ok(a) => a,
                    Err(e) => {
                        log::warn!(
                            "Skip registry for {} (e.g. UE5.6 format): {:#}. File will still be written as raw bytes.",
                            pak_path, e
                        );
                        continue;
                    }
                };
                if let Err(e) = self
                    .asset_registry
                    .populate(normalized.with_extension("").to_str().unwrap(), &asset)
                {
                    log::warn!("Skip registry populate for {}: {:#}", pak_path, e);
                }
            }
        }
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

    fn serialize_asset_registry(&mut self) -> Result<()> {
        // RC 生成 AssetRegistry.bin 的逻辑暂时屏蔽（UE 5.6 格式兼容未就绪）
        // let mut buf = Vec::new();
        // self.asset_registry
        //     .write(&mut buf)
        //     .context("Failed to serialize asset registry")?;
        // self.bundle
        //     .write_file(&buf, self.installation.asset_registry_pak_path())
        //     .context("Failed to write asset registry to mod pak")?;
        Ok(())
    }

}
