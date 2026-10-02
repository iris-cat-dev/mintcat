use crate::uasset_utils::paths::PakPathBuf;
use anyhow::{Context, Result};
use mintcat_unreal_pak::{PakBuildCodec, PakBuildTarget, PakBundleWriter};
use std::io::{Cursor, Read, Seek, Write};
use unreal_asset::Asset;

pub struct ModBundleWriter<W: Write + Seek> {
    inner: PakBundleWriter<W>,
}

impl<W: Write + Seek> ModBundleWriter<W> {
    pub fn new(writer: W, reference_paths: &[String], compress: bool) -> Result<Self> {
        let target = PakBuildTarget {
            codec: if compress {
                PakBuildCodec::Zlib
            } else {
                PakBuildCodec::None
            },
            ..PakBuildTarget::default()
        };
        Ok(Self {
            inner: PakBundleWriter::new(writer, reference_paths, &target)
                .context("Failed to initialize Unreal PAK writer")?,
        })
    }

    pub fn normalize_path(&self, path: &str) -> PakPathBuf {
        self.inner
            .normalize_case(path)
            .expect("legacy caller supplied an invalid PAK path")
    }

    pub fn write_file(&mut self, data: &[u8], path: &str) -> Result<()> {
        self.inner
            .write_file(path, data)
            .with_context(|| format!("Failed to write file to pak: {path}"))?;
        Ok(())
    }

    pub fn write_asset<C: Read + Seek>(&mut self, asset: Asset<C>, path: &str) -> Result<()> {
        let mut data_out = (Cursor::new(Vec::new()), Cursor::new(Vec::new()));
        asset
            .write_data(&mut data_out.0, Some(&mut data_out.1))
            .with_context(|| format!("Failed to serialize asset: {path}"))?;
        self.write_file(data_out.0.get_ref(), &format!("{path}.uasset"))?;
        self.write_file(data_out.1.get_ref(), &format!("{path}.uexp"))?;
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        self.inner.finish().context("Failed to write PAK index")?;
        Ok(())
    }
}
