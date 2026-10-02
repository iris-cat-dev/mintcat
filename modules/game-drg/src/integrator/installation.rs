use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct DRGInstallation {
    pub pak_path: PathBuf,
    pub root: PathBuf,
}

#[derive(Debug)]
pub enum DRGInstallationType {
    Steam,
    Xbox,
}

impl DRGInstallation {
    pub fn from_pak_path<P: AsRef<Path>>(pak_path: P) -> Result<Self> {
        let pak = pak_path.as_ref();
        let root = pak
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .with_context(|| format!("Failed to get pak parent directory: {:?}", pak))?
            .to_path_buf();
        Ok(Self {
            pak_path: pak.to_path_buf(),
            root,
        })
    }

    pub fn install_type(&self) -> DRGInstallationType {
        let pak_name = self.pak_path.file_name().unwrap();
        let name_str = pak_name.to_str().expect("Invalid UTF-8 path name");
        match name_str {
            "FSD-WindowsNoEditor.pak" => DRGInstallationType::Steam,
            "FSD-WinGDK.pak" => DRGInstallationType::Xbox,
            _ => panic!("Unknown DRG installation type (not FSD): {}", name_str),
        }
    }

    pub fn mod_pak_name(&self) -> PathBuf {
        match self.install_type() {
            DRGInstallationType::Steam => "FSD-WindowsNoEditor_Mods.pak".parse().unwrap(),
            DRGInstallationType::Xbox => "FSD-WinGDK_Mods.pak".parse().unwrap(),
        }
    }

    /// Pak 内 AssetRegistry 路径（仅 FSD）
    pub fn asset_registry_pak_path(&self) -> &'static str {
        "FSD/AssetRegistry.bin"
    }

    /// Content 前缀，用于 unpacked mod 路径：FSD/Content/...
    pub fn content_prefix(&self) -> &'static str {
        "FSD"
    }

    /// 用于 strip_prefix 的 content 路径
    pub fn content_prefix_for_path(&self) -> &'static str {
        "FSD/Content"
    }

    pub fn binaries_directory(&self) -> PathBuf {
        self.root.join("Binaries").join("Win64")
    }

    pub fn paks_path(&self) -> PathBuf {
        self.root.join("Content").join("Paks")
    }

    #[allow(dead_code)]
    pub fn main_pak(&self) -> PathBuf {
        self.root
            .join("Content")
            .join("Paks")
            .join("FSD-WindowsNoEditor.pak")
    }

    #[allow(dead_code)]
    pub fn modio_directory(&self) -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            Some(PathBuf::from("C:\\Users\\Public\\mod.io\\2475"))
        }
        #[cfg(target_os = "linux")]
        {
            steamlocate::SteamDir::locate()
                .map(|s| {
                    s.path()
                        .join("steamapps/compatdata/548430/pfx/drive_c/users/Public/mod.io/2475")
                })
                .ok()
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        {
            None // TODO
        }
    }
}
