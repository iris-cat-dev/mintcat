use crate::common::zip::{extract_zip_to_directory, is_valid_zip_file};
use crate::ReadSeek;
use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use zip::read::ZipArchive;

fn sanitize_dir_name(input: &str) -> String {
    let mut s: String = input
        .chars()
        .map(|c| {
            if c.is_control()
                || std::path::is_separator(c)
                || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')
            {
                '_'
            } else {
                c
            }
        })
        .collect();

    s = s.trim().trim_matches('.').to_string();
    if s.is_empty() {
        s = "mod".to_string();
    }

    let base = s.split('.').next().unwrap_or(&s);
    let base_upper = base.to_ascii_uppercase();
    let is_reserved = matches!(base_upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || base_upper
            .strip_prefix("COM")
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=9).contains(&n))
        || base_upper
            .strip_prefix("LPT")
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=9).contains(&n));

    if is_reserved {
        format!("_{}", s)
    } else {
        s
    }
}

/// Install UE4SSL by extracting the entire zip to the game directory.
/// Preserves the archive's directory structure. Ensures ue4ss/mods exists for mod loading.
/// Validates the ZIP archive before extraction to provide clear error messages.
pub fn install_ue4ss(install_path: &Path, ue4ss_zip_path: Option<&Path>) -> Result<()> {
    let zip_path = ue4ss_zip_path
        .ok_or_else(|| anyhow::anyhow!("UE4SSL asset zip path is required (UE4SSL.zip)"))?;

    let install_path_str = install_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid install path"))?;
    let zip_path_str = zip_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid zip path"))?;

    if !zip_path.exists() {
        anyhow::bail!(
            "UE4SS zip file not found: {:?}. Please re-download the asset.",
            zip_path
        );
    }

    if !is_valid_zip_file(zip_path_str) {
        let file_size = fs::metadata(zip_path).map(|m| m.len()).unwrap_or(0);
        log::error!(
            "UE4SS zip is corrupted or incomplete: {:?} (size: {} bytes)",
            zip_path,
            file_size
        );
        anyhow::bail!(
            "UE4SS zip file is corrupted (size: {} bytes). The cached artifact was preserved for diagnosis.",
            file_size
        );
    }

    log::info!("Installing UE4SS: extracting zip to {:?}", install_path);

    extract_zip_to_directory(zip_path_str, install_path_str)
        .map_err(|e| anyhow::anyhow!("Failed to extract UE4SSL zip to game directory: {}", e))?;

    let mods_path = install_path.join("ue4ss").join("mods");
    if !mods_path.exists() {
        fs::create_dir_all(&mods_path)
            .with_context(|| format!("Failed to create ue4ss mods directory: {:?}", mods_path))?;
    }

    Ok(())
}

pub fn ensure_ue4ss_config_directory(install_path: &Path) -> Result<()> {
    let ue4ss_dir = install_path.join("ue4ss");
    let config_dir = ue4ss_dir.join("config");

    fs::create_dir_all(&config_dir)
        .with_context(|| format!("Failed to create ue4ss config directory: {:?}", config_dir))?;
    Ok(())
}

pub fn ensure_ue4ss_settings_file(
    install_path: &Path,
    required_sections: &[(&str, &[(&str, &str)])],
) -> Result<()> {
    let ue4ss_dir = install_path.join("ue4ss");
    fs::create_dir_all(&ue4ss_dir)
        .with_context(|| format!("Failed to create ue4ss directory: {:?}", ue4ss_dir))?;

    let settings_path = ue4ss_dir.join("UE4SS-settings.ini");
    let current = if settings_path.exists() {
        fs::read_to_string(&settings_path)
            .with_context(|| format!("Failed to read UE4SS settings file: {:?}", settings_path))?
    } else {
        String::new()
    };

    if let Some(updated) = ensure_ini_entries(&current, required_sections) {
        fs::write(&settings_path, updated)
            .with_context(|| format!("Failed to write UE4SS settings file: {:?}", settings_path))?;
    }

    Ok(())
}

fn ensure_ini_entries(
    content: &str,
    required_sections: &[(&str, &[(&str, &str)])],
) -> Option<String> {
    let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    let mut changed = false;

    for (section, entries) in required_sections {
        changed |= ensure_ini_section_entries(&mut lines, section, entries);
    }

    if !changed {
        return None;
    }

    let mut updated = lines.join("\n");
    if !updated.is_empty() {
        updated.push('\n');
    }
    Some(updated)
}

fn ensure_ini_section_entries(
    lines: &mut Vec<String>,
    section: &str,
    entries: &[(&str, &str)],
) -> bool {
    let Some((start, end)) = find_ini_section_bounds(lines, section) else {
        if !lines.is_empty() && lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("[{}]", section));
        lines.extend(
            entries
                .iter()
                .map(|(key, value)| format!("{key} = {value}")),
        );
        return true;
    };

    let mut changed = false;
    let mut missing = Vec::new();
    for (key, value) in entries {
        let desired = format!("{key} = {value}");
        if let Some(index) = find_ini_key_index(&lines[start + 1..end], key) {
            let line = &mut lines[start + 1 + index];
            if parse_ini_value(line) != Some(*value) {
                *line = desired;
                changed = true;
            }
        } else {
            missing.push(desired);
        }
    }

    if !missing.is_empty() {
        let mut insert_at = end;
        while insert_at > start + 1 && lines[insert_at - 1].trim().is_empty() {
            insert_at -= 1;
        }
        lines.splice(insert_at..insert_at, missing);
        changed = true;
    }

    changed
}

fn find_ini_section_bounds(lines: &[String], section: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|line| {
        parse_ini_section(line).is_some_and(|current| current.eq_ignore_ascii_case(section))
    })?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, line)| parse_ini_section(line).map(|_| index))
        .unwrap_or(lines.len());
    Some((start, end))
}

fn find_ini_key_index(lines: &[String], key: &str) -> Option<usize> {
    lines.iter().position(|line| {
        parse_ini_key(line).is_some_and(|current| current.eq_ignore_ascii_case(key))
    })
}

fn parse_ini_section(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('[') && trimmed.ends_with(']') {
        Some(trimmed[1..trimmed.len() - 1].trim())
    } else {
        None
    }
}

fn parse_ini_key(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.starts_with(';') || trimmed.starts_with('#') {
        return None;
    }
    trimmed.split_once('=').map(|(key, _)| key.trim())
}

fn parse_ini_value(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.starts_with(';') || trimmed.starts_with('#') {
        return None;
    }
    trimmed.split_once('=').map(|(_, value)| value.trim())
}

/// Returns true if the zip contains a JS script mod (e.g. path ending with `js/main.js`).
pub fn zip_contains_js_mod(path: &Path) -> bool {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut archive = match ZipArchive::new(file) {
        Ok(a) => a,
        Err(_) => return false,
    };
    for i in 0..archive.len() {
        let entry = match archive.by_index(i) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.name().replace('\\', "/").to_lowercase();
        if name.ends_with("js/main.js") {
            return true;
        }
    }
    false
}

/// Install JS mod from a zip, extracting only the js/ content to ue4ss/mods/{name}/js/.
/// Works for both pure JS zips and mixed zips (containing pak + js).
pub fn install_ue4ss_js_mod_from_zip_targeted(
    install_path: &Path,
    mod_name: &str,
    zip_path: &Path,
) -> Result<()> {
    let mods_dir = install_path.join("ue4ss").join("mods");
    let sanitized = sanitize_dir_name(mod_name);
    let mod_dir = mods_dir.join(&sanitized);

    let file =
        File::open(zip_path).with_context(|| format!("Failed to open zip: {:?}", zip_path))?;
    let mut archive = ZipArchive::new(file).context("Failed to parse zip")?;

    let mut js_base = String::new();
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .with_context(|| format!("Failed to read zip entry at index {}", i))?;
        let name = entry.name().replace('\\', "/");
        let lower = name.to_lowercase();
        if lower.ends_with("js/main.js") {
            if let Some(pos) = lower.rfind("js/main.js") {
                js_base = name[..pos].to_string();
            }
            break;
        }
    }

    let js_prefix = format!("{}js/", js_base);
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .with_context(|| format!("Failed to read zip entry at index {}", i))?;
        let name = entry.name().replace('\\', "/");
        if !name.starts_with(&js_prefix) {
            continue;
        }
        let relative = &name[js_base.len()..];
        let target = mod_dir.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            let mut out = File::create(&target)?;
            out.write_all(&content)?;
        }
    }

    log::info!("Installed JS mod from zip: {:?} -> {:?}", zip_path, mod_dir);
    Ok(())
}

/// Check if a directory contains a JS script mod (js/main.js).
pub fn dir_contains_js_mod(path: &Path) -> bool {
    path.join("js").join("main.js").exists()
}

/// Install JS mod from a directory by copying the js/ subfolder to ue4ss/mods/{name}/js/.
pub fn install_ue4ss_js_mod_from_dir(
    install_path: &Path,
    mod_name: &str,
    mod_dir: &Path,
) -> Result<()> {
    let mods_dir = install_path.join("ue4ss").join("mods");
    let sanitized = sanitize_dir_name(mod_name);
    let target_mod_dir = mods_dir.join(&sanitized);
    let target_js_dir = target_mod_dir.join("js");
    let source_js_dir = mod_dir.join("js");

    fs::create_dir_all(&target_js_dir)
        .with_context(|| format!("Failed to create JS mod directory: {:?}", target_js_dir))?;
    copy_dir_recursive(&source_js_dir, &target_js_dir)
        .with_context(|| format!("Failed to copy JS mod from {:?}", source_js_dir))?;

    log::info!(
        "Installed JS mod from directory: {:?} -> {:?}",
        source_js_dir,
        target_js_dir
    );
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }
    for entry in
        fs::read_dir(src).with_context(|| format!("Failed to read directory: {:?}", src))?
    {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)
                .with_context(|| format!("Failed to copy {:?} to {:?}", src_path, dst_path))?;
        }
    }
    Ok(())
}

pub fn install_ue4ss_mod(
    install_path: &Path,
    mod_name: &str,
    mod_data: &mut Box<dyn ReadSeek>,
) -> Result<()> {
    let mods_home_path = install_path.join("ue4ss").join("mods");

    let sanitized_mod_name = sanitize_dir_name(mod_name);
    let mod_path = mods_home_path.join(&sanitized_mod_name);

    if !mod_path.exists() {
        fs::create_dir_all(&mod_path)
            .with_context(|| format!("Failed to create mod directory {:?}", mod_path))?;
    }

    let dll_path = mod_path.join("main.dll");
    let mut content = Vec::new();
    mod_data
        .read_to_end(&mut content)
        .context("Failed to read mod data")?;

    let temp_path = dll_path.with_extension("dll.tmp");
    {
        let mut file =
            fs::File::create(&temp_path).context("Failed to create temp file for mod")?;
        file.write_all(&content)
            .context("Failed to write mod content")?;
        file.flush().context("Failed to flush mod file")?;
    }

    if dll_path.exists() {
        let _ = fs::remove_file(&dll_path);
    }
    fs::rename(&temp_path, &dll_path).context("Failed to rename mod file")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ensure_ue4ss_config_directory, ensure_ue4ss_settings_file, install_ue4ss};
    use std::fs;

    #[test]
    fn ensure_ue4ss_config_directory_creates_config_dir() {
        let temp_dir = tempfile::tempdir().unwrap();

        ensure_ue4ss_config_directory(temp_dir.path()).unwrap();

        assert!(temp_dir.path().join("ue4ss").join("config").is_dir());
    }

    #[test]
    fn ensure_ue4ss_config_directory_works_with_existing_ue4ss_dir() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp_dir.path().join("ue4ss")).unwrap();

        ensure_ue4ss_config_directory(temp_dir.path()).unwrap();

        assert!(temp_dir.path().join("ue4ss").join("config").is_dir());
    }

    #[test]
    fn ensure_ue4ss_settings_file_does_not_duplicate_existing_keys() {
        let temp_dir = tempfile::tempdir().unwrap();

        ensure_ue4ss_settings_file(
            temp_dir.path(),
            &[("Hooks", &[("HookProcessInternal", "1")])],
        )
        .unwrap();
        ensure_ue4ss_settings_file(
            temp_dir.path(),
            &[("Hooks", &[("HookProcessInternal", "1")])],
        )
        .unwrap();

        let settings =
            fs::read_to_string(temp_dir.path().join("ue4ss").join("UE4SS-settings.ini")).unwrap();
        assert_eq!(settings.matches("HookProcessInternal").count(), 1);
    }

    #[test]
    fn invalid_ue4ss_archive_is_never_deleted() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive = temp_dir.path().join("UE4SS.zip");
        fs::write(&archive, b"not a zip").unwrap();

        assert!(install_ue4ss(temp_dir.path(), Some(&archive)).is_err());
        assert_eq!(fs::read(archive).unwrap(), b"not a zip");
    }

}
