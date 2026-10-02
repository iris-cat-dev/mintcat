use mintcat_game_api::{
    GameDescriptor, GameInputFormat, GameResourceRequirement, GameSourceBinding,
    GameVariantDescriptor, GAME_ADAPTER_CONTRACT_ID, GAME_ADAPTER_CONTRACT_VERSION,
};
use mintcat_sdk::{
    ModuleActivation, ModuleComponent, ModuleFile, ModuleManifest, ModuleState, PermissionRequest,
    ProvidedContract, RequiredContract,
};
use serde_json::json;

use crate::{DRG_GAME_ID, DRG_MODULE_ID, DRG_MODULE_VERSION};

pub const STEAM_VARIANT_ID: &str = "steam";
pub const XBOX_VARIANT_ID: &str = "xbox";
pub const STEAM_APP_ID: &str = "548430";
pub const STEAM_MAIN_PAK: &str = "FSD/Content/Paks/FSD-WindowsNoEditor.pak";
pub const XBOX_MAIN_PAK: &str = "FSD/Content/Paks/FSD-WinGDK.pak";
pub const STEAM_MOD_PAK: &str = "FSD/Content/Paks/FSD-WindowsNoEditor_Mods.pak";
pub const XBOX_MOD_PAK: &str = "FSD/Content/Paks/FSD-WinGDK_Mods.pak";
pub const PAKS_PATH: &str = "FSD/Content/Paks";
pub const BINARIES_PATH: &str = "FSD/Binaries/Win64";
pub const DRG_PATCH_RESOURCE_ID: &str = "resource.drg.patch";
pub const UE4SS_RESOURCE_ID: &str = "resource.ue4ss";

pub fn descriptor() -> GameDescriptor {
    let descriptor = GameDescriptor {
        schema_version: 1,
        game_id: DRG_GAME_ID.to_owned(),
        display_name: "Deep Rock Galactic".to_owned(),
        adapter_version: DRG_MODULE_VERSION.to_owned(),
        settings_schema_version: 1,
        settings_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["compression", "ue4ssEnabled"],
            "properties": {
                "compression": {"type": "string", "enum": ["none", "zlib"]},
                "ue4ssEnabled": {"type": "boolean"}
            }
        }),
        variants: vec![
            GameVariantDescriptor {
                variant_id: STEAM_VARIANT_ID.to_owned(),
                display_name: "Steam".to_owned(),
                store: "steam".to_owned(),
                store_app_id: Some(STEAM_APP_ID.to_owned()),
                target: "x86_64-pc-windows-msvc".to_owned(),
                main_pak_relative_path: STEAM_MAIN_PAK.to_owned(),
                paks_relative_path: PAKS_PATH.to_owned(),
                binaries_relative_path: BINARIES_PATH.to_owned(),
                mod_pak_relative_path: STEAM_MOD_PAK.to_owned(),
                process_names: vec!["FSD-Win64-Shipping.exe".to_owned(), "FSD.exe".to_owned()],
            },
            GameVariantDescriptor {
                variant_id: XBOX_VARIANT_ID.to_owned(),
                display_name: "Microsoft Store / Xbox".to_owned(),
                store: "microsoft-store".to_owned(),
                store_app_id: None,
                target: "x86_64-pc-windows-msvc".to_owned(),
                main_pak_relative_path: XBOX_MAIN_PAK.to_owned(),
                paks_relative_path: PAKS_PATH.to_owned(),
                binaries_relative_path: BINARIES_PATH.to_owned(),
                mod_pak_relative_path: XBOX_MOD_PAK.to_owned(),
                process_names: vec!["FSD-WinGDK-Shipping.exe".to_owned(), "FSD.exe".to_owned()],
            },
        ],
        input_formats: vec![
            GameInputFormat::Pak,
            GameInputFormat::Zip,
            GameInputFormat::DirectorySnapshot,
        ],
        source_bindings: vec![
            GameSourceBinding {
                source_id: "modio".to_owned(),
                remote_game_key: "drg".to_owned(),
                provider_game_id: "2475".to_owned(),
            },
            GameSourceBinding {
                source_id: "nexusmods".to_owned(),
                remote_game_key: "drg".to_owned(),
                provider_game_id: "deeprockgalactic".to_owned(),
            },
            GameSourceBinding {
                source_id: "modcat".to_owned(),
                remote_game_key: "drg".to_owned(),
                provider_game_id: "drg".to_owned(),
            },
            GameSourceBinding {
                source_id: "local".to_owned(),
                remote_game_key: "drg".to_owned(),
                provider_game_id: "drg".to_owned(),
            },
        ],
        resources: vec![
            GameResourceRequirement {
                resource_id: DRG_PATCH_RESOURCE_ID.to_owned(),
                version_range: ">=1,<2".to_owned(),
                optional: false,
                purpose: "signed DRG integration assets and x3daudio hook".to_owned(),
            },
            GameResourceRequirement {
                resource_id: UE4SS_RESOURCE_ID.to_owned(),
                version_range: ">=3,<4".to_owned(),
                optional: true,
                purpose: "UE4SS loader selected by the per-game settings snapshot".to_owned(),
            },
        ],
        capabilities: vec![
            "discover".to_owned(),
            "validate-installation".to_owned(),
            "inspect-inputs".to_owned(),
            "create-build-plan".to_owned(),
            "build-legacy-compatible-pak".to_owned(),
            "create-deploy-plan".to_owned(),
            "detect-legacy-install".to_owned(),
        ],
    };
    descriptor
        .validate()
        .expect("static DRG descriptor must satisfy game contract");
    descriptor
}

pub fn module_manifest(target: &str, host_api: &str, worker: ModuleFile) -> ModuleManifest {
    ModuleManifest {
        schema_version: 1,
        id: DRG_MODULE_ID.to_owned(),
        version: DRG_MODULE_VERSION.to_owned(),
        publisher: "mintcat.official".to_owned(),
        target: target.to_owned(),
        host_api: host_api.to_owned(),
        components: vec![ModuleComponent {
            kind: "service".to_owned(),
            entry: worker.path.clone(),
            runtime: "worker".to_owned(),
            state_model: "stateless".to_owned(),
        }],
        provides: vec![ProvidedContract {
            contract_id: GAME_ADAPTER_CONTRACT_ID.to_owned(),
            version: GAME_ADAPTER_CONTRACT_VERSION,
            cardinality: Some("multi".to_owned()),
        }],
        requires: vec![RequiredContract {
            contract_id: "engine.unreal-pak".to_owned(),
            version_range: "=1".to_owned(),
            optional: false,
            binding_scope: None,
        }],
        permissions: vec![
            PermissionRequest {
                capability: "artifact.read".to_owned(),
                scope: "operation-grants".to_owned(),
            },
            PermissionRequest {
                capability: "staging.write".to_owned(),
                scope: "operation-staging".to_owned(),
            },
            PermissionRequest {
                capability: "installation.read".to_owned(),
                scope: "selected:game:drg".to_owned(),
            },
        ],
        contributes: json!({
            "games": [descriptor()],
            "scopeId": "game:drg"
        }),
        state: ModuleState {
            schema_version: 1,
            readable_range: "1".to_owned(),
            writable_range: "1".to_owned(),
            rollback_before_write_only: Some(true),
        },
        activation: ModuleActivation {
            trigger: "on-demand".to_owned(),
            drain_policy: "finish-requests".to_owned(),
        },
        files: vec![worker],
    }
}
