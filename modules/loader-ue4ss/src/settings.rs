use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::LoaderError;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ue4ssSettings {
    required_sections: BTreeMap<String, BTreeMap<String, String>>,
}

impl Ue4ssSettings {
    pub fn deep_rock_galactic() -> Self {
        Self::default()
    }

    pub fn rogue_core() -> Self {
        Self::default()
            .with_entry("EngineVersionOverride", "MajorVersion", "5")
            .with_entry("EngineVersionOverride", "MinorVersion", "6")
            .with_entry("Hooks", "HookProcessInternal", "1")
            .with_entry("Hooks", "HookProcessLocalScriptFunction", "1")
    }

    pub fn with_entry(mut self, section: &str, key: &str, value: &str) -> Self {
        self.required_sections
            .entry(section.to_owned())
            .or_default()
            .insert(key.to_owned(), value.to_owned());
        self
    }

    pub fn is_empty(&self) -> bool {
        self.required_sections.is_empty()
    }

    pub(crate) fn apply(&self, content: &[u8]) -> Result<Vec<u8>, LoaderError> {
        let content = std::str::from_utf8(content).map_err(|_| {
            LoaderError::InvalidRequest("signed UE4SS settings resource must be UTF-8".to_owned())
        })?;
        if self.required_sections.is_empty() {
            return Ok(content.as_bytes().to_vec());
        }
        validate_settings(self)?;
        let mut lines = content.lines().map(str::to_owned).collect::<Vec<_>>();
        let mut changed = false;
        for (section, entries) in &self.required_sections {
            changed |= ensure_section_entries(&mut lines, section, entries);
        }
        if !changed {
            return Ok(content.as_bytes().to_vec());
        }
        let mut updated = lines.join("\n");
        if !updated.is_empty() {
            updated.push('\n');
        }
        Ok(updated.into_bytes())
    }
}

fn validate_settings(settings: &Ue4ssSettings) -> Result<(), LoaderError> {
    for (section, entries) in &settings.required_sections {
        if section.trim().is_empty()
            || section.contains(['[', ']', '\n', '\r'])
            || entries.is_empty()
        {
            return Err(LoaderError::InvalidRequest(
                "UE4SS setting sections must be non-empty INI names".to_owned(),
            ));
        }
        for (key, value) in entries {
            if key.trim().is_empty()
                || key.contains(['=', '\n', '\r'])
                || value.contains(['\n', '\r'])
            {
                return Err(LoaderError::InvalidRequest(
                    "UE4SS setting keys and values must fit on one INI line".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn ensure_section_entries(
    lines: &mut Vec<String>,
    section: &str,
    entries: &BTreeMap<String, String>,
) -> bool {
    let Some((start, end)) = find_section_bounds(lines, section) else {
        if !lines.is_empty() && lines.last().is_some_and(|line| !line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(format!("[{section}]"));
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
        if let Some(index) = find_key_index(&lines[start + 1..end], key) {
            let line = &mut lines[start + 1 + index];
            if parse_value(line) != Some(value.as_str()) {
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

fn find_section_bounds(lines: &[String], section: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|line| {
        parse_section(line).is_some_and(|current| current.eq_ignore_ascii_case(section))
    })?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, line)| parse_section(line).map(|_| index))
        .unwrap_or(lines.len());
    Some((start, end))
}

fn find_key_index(lines: &[String], key: &str) -> Option<usize> {
    lines
        .iter()
        .position(|line| parse_key(line).is_some_and(|current| current.eq_ignore_ascii_case(key)))
}

fn parse_section(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    (trimmed.len() >= 2 && trimmed.starts_with('[') && trimmed.ends_with(']'))
        .then(|| trimmed[1..trimmed.len() - 1].trim())
}

fn parse_key(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.starts_with(';') || trimmed.starts_with('#') {
        return None;
    }
    trimmed.split_once('=').map(|(key, _)| key.trim())
}

fn parse_value(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.starts_with(';') || trimmed.starts_with('#') {
        return None;
    }
    trimmed.split_once('=').map(|(_, value)| value.trim())
}

#[cfg(test)]
mod tests {
    use super::Ue4ssSettings;

    #[test]
    fn rogue_core_overrides_only_required_values_deterministically() {
        let source = b"; keep\n[Hooks]\nOther = yes\nHookProcessInternal = 0\n";
        let expected = concat!(
            "; keep\n",
            "[Hooks]\n",
            "Other = yes\n",
            "HookProcessInternal = 1\n",
            "HookProcessLocalScriptFunction = 1\n",
            "\n",
            "[EngineVersionOverride]\n",
            "MajorVersion = 5\n",
            "MinorVersion = 6\n",
        );
        let first = Ue4ssSettings::rogue_core().apply(source).unwrap();
        assert_eq!(first, expected.as_bytes());
        assert_eq!(Ue4ssSettings::rogue_core().apply(&first).unwrap(), first);
        assert_eq!(
            Ue4ssSettings::deep_rock_galactic().apply(source).unwrap(),
            source
        );
    }
}
