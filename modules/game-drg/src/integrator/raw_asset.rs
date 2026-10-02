use anyhow::{Context, Result};
use unreal_asset::engine_version::EngineVersion;
use unreal_asset::{Asset, AssetBuilder};

#[derive(Debug, Default)]
pub struct RawAsset {
    pub uasset: Option<Vec<u8>>,
    pub uexp: Option<Vec<u8>>,
}

impl RawAsset {
    pub fn parse(&self) -> Result<Asset<std::io::Cursor<&Vec<u8>>>> {
        let uasset_data = self
            .uasset
            .as_ref()
            .context("Missing uasset data for raw asset")?;
        let uexp_data = self
            .uexp
            .as_ref()
            .context("Missing uexp data for raw asset")?;

        let asset = AssetBuilder::new(std::io::Cursor::new(uasset_data), EngineVersion::VER_UE4_27)
            .bulk(std::io::Cursor::new(uexp_data))
            .build()
            .context("Failed to parse raw asset")?;

        Ok(asset)
    }
}
