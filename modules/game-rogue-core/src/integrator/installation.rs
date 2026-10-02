//! Rogue Core (UE 5.6) 安装路径与 pak 路径常量

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Rogue Core 安装目录信息
#[derive(Debug, Default)]
pub struct RcInstallation {
    pub pak_path: PathBuf,
    pub root: PathBuf,
}

impl RcInstallation {
    /// 从主 pak 路径解析安装目录，仅接受 RogueCore-Windows.pak
    pub fn from_pak_path<P: AsRef<Path>>(pak_path: P) -> Result<Self> {
        let pak = pak_path.as_ref();
        let name = pak
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("Invalid pak path: {:?}", pak))?;
        if name != "RogueCore-Windows.pak" {
            anyhow::bail!(
                "Not a Rogue Core pak path (expected RogueCore-Windows.pak): {:?}",
                pak
            );
        }
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

    pub fn mod_pak_name(&self) -> PathBuf {
        PathBuf::from("RogueCore-Windows_Mods.pak")
    }

    pub fn asset_registry_pak_path(&self) -> &'static str {
        "RogueCore/AssetRegistry.bin"
    }

    /// Content 前缀，用于 unpacked mod 路径：{prefix}/Content/...
    pub fn content_prefix(&self) -> &'static str {
        "RogueCore"
    }

    pub fn binaries_directory(&self) -> PathBuf {
        self.root.join("Binaries").join("Win64")
    }

    pub fn paks_path(&self) -> PathBuf {
        self.root.join("Content").join("Paks")
    }
}
