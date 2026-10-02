use anyhow::{Context, Result};
use std::io::Cursor;
use std::path::Path;
use zip::read::ZipArchive;

pub const AUDIO_PAK_INFIX: &str = "_audio_";
pub const AUDIO_PAK_SUFFIX: &str = "_P.pak";

pub fn sanitize_mod_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn audio_pak_filename(game_pak_stem: &str, mod_name: &str) -> String {
    format!(
        "{}{}{}{}",
        game_pak_stem,
        AUDIO_PAK_INFIX,
        sanitize_mod_name(mod_name),
        AUDIO_PAK_SUFFIX
    )
}

pub fn is_mintcat_audio_pak(filename: &str) -> bool {
    filename.contains(AUDIO_PAK_INFIX) && filename.ends_with(AUDIO_PAK_SUFFIX)
}


pub fn verify_audio_only_from_bytes(pak_data: &[u8]) -> Result<bool> {
    let mut cursor = Cursor::new(pak_data);
    let pak = repak::PakBuilder::new()
        .reader(&mut cursor)
        .context("Failed to parse pak for audio verification")?;

    for file_path in pak.files() {
        let lower = file_path.to_lowercase();
        if lower.ends_with("initspacerig.uasset")
            || lower.ends_with("initcave.uasset")
            || lower.ends_with("assetregistry.bin")
            || lower.ends_with(".ushaderbytecode")
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn verify_audio_only_pak_file(path: &Path) -> Result<bool> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open pak for audio verification: {:?}", path))?;
    let mut reader = std::io::BufReader::new(file);
    let pak = repak::PakBuilder::new()
        .reader(&mut reader)
        .context("Failed to parse pak for audio verification")?;

    for file_path in pak.files() {
        let lower = file_path.to_lowercase();
        if lower.ends_with("initspacerig.uasset")
            || lower.ends_with("initcave.uasset")
            || lower.ends_with("assetregistry.bin")
            || lower.ends_with(".ushaderbytecode")
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn zip_contains_file_name(path: &Path, file_name: &str) -> Result<bool> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open zip for verification: {:?}", path))?;
    let mut archive = ZipArchive::new(file).context("Failed to parse zip for verification")?;

    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .with_context(|| format!("Failed to inspect zip entry at index {}", i))?;
        let entry_name = entry.name().replace('\\', "/");
        if Path::new(&entry_name)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.eq_ignore_ascii_case(file_name))
            .unwrap_or(false)
        {
            return Ok(true);
        }
    }

    Ok(false)
}
