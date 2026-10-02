use std::path::Path;

use anyhow::Result;
use mintcat_integrator_core::common::ue4ss::ensure_ue4ss_settings_file;

pub(super) fn ensure_rogue_core_ue4ss_settings_file(install_path: &Path) -> Result<()> {
    ensure_ue4ss_settings_file(
        install_path,
        &[
            (
                "EngineVersionOverride",
                &[("MajorVersion", "5"), ("MinorVersion", "6")],
            ),
            (
                "Hooks",
                &[
                    ("HookProcessInternal", "1"),
                    ("HookProcessLocalScriptFunction", "1"),
                ],
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::ensure_rogue_core_ue4ss_settings_file;

    #[test]
    fn ensure_rogue_core_ue4ss_settings_file_creates_required_settings() {
        let temp_dir = tempfile::tempdir().unwrap();

        ensure_rogue_core_ue4ss_settings_file(temp_dir.path()).unwrap();

        let settings =
            fs::read_to_string(temp_dir.path().join("ue4ss").join("UE4SS-settings.ini")).unwrap();
        assert!(settings.contains("[EngineVersionOverride]"));
        assert!(settings.contains("MajorVersion = 5"));
        assert!(settings.contains("MinorVersion = 6"));
        assert!(settings.contains("[Hooks]"));
        assert!(settings.contains("HookProcessInternal = 1"));
        assert!(settings.contains("HookProcessLocalScriptFunction = 1"));
    }

    #[test]
    fn ensure_ue4ss_settings_file_updates_managed_values_and_adds_missing_keys() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ue4ss_dir = temp_dir.path().join("ue4ss");
        fs::create_dir_all(&ue4ss_dir).unwrap();
        let settings_path = ue4ss_dir.join("UE4SS-settings.ini");
        fs::write(
            &settings_path,
            "[General]\nConsoleEnabled = 0\n\n[EngineVersionOverride]\nMajorVersion = 4\n\n[Hooks]\nHookProcessInternal = 0\nHookCallFunctionByNameWithArguments = 0\n",
        )
        .unwrap();

        ensure_rogue_core_ue4ss_settings_file(temp_dir.path()).unwrap();

        let settings = fs::read_to_string(settings_path).unwrap();
        assert!(settings.contains("ConsoleEnabled = 0"));
        assert!(settings.contains("HookCallFunctionByNameWithArguments = 0"));
        assert!(settings.contains("MajorVersion = 5"));
        assert!(settings.contains("MinorVersion = 6"));
        assert!(settings.contains("HookProcessInternal = 1"));
        assert!(settings.contains("HookProcessLocalScriptFunction = 1"));
    }
}
