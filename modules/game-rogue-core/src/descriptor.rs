use mintcat_game_api::{
    GameDescriptor, GameInputFormat, GameResourceRequirement, GameSourceBinding,
    GameVariantDescriptor, GAME_ADAPTER_CONTRACT_ID, GAME_ADAPTER_CONTRACT_VERSION,
};
use mintcat_sdk::{
    ModuleActivation, ModuleComponent, ModuleFile, ModuleManifest, ModuleState, PermissionRequest,
    ProvidedContract, RequiredContract,
};
use serde_json::json;

use crate::{ROGUE_CORE_GAME_ID, ROGUE_CORE_MODULE_ID, ROGUE_CORE_MODULE_VERSION};

pub const RELEASE_VARIANT_ID: &str = "steam-release";
pub const PLAYTEST_VARIANT_ID: &str = "steam-playtest";
pub const RELEASE_APP_ID: &str = "2605790";
pub const PLAYTEST_APP_ID: &str = "2860770";
pub const MAIN_PAK: &str = "Content/Paks/RogueCore-Windows.pak";
pub const MOD_PAK: &str = "Content/Paks/RogueCore-Windows_Mods.pak";
pub const PAKS_PATH: &str = "Content/Paks";
pub const BINARIES_PATH: &str = "Binaries/Win64";
pub const ROGUE_CORE_PATCH_RESOURCE_ID: &str = "resource.rogue-core.patch";
pub const UE4SS_RESOURCE_ID: &str = "resource.ue4ss";

fn variant(variant_id: &str, display_name: &str, app_id: &str) -> GameVariantDescriptor {
    GameVariantDescriptor {
        variant_id: variant_id.to_owned(),
        display_name: display_name.to_owned(),
        store: "steam".to_owned(),
        store_app_id: Some(app_id.to_owned()),
        target: "x86_64-pc-windows-msvc".to_owned(),
        main_pak_relative_path: MAIN_PAK.to_owned(),
        paks_relative_path: PAKS_PATH.to_owned(),
        binaries_relative_path: BINARIES_PATH.to_owned(),
        mod_pak_relative_path: MOD_PAK.to_owned(),
        process_names: vec![
            "RogueCore-Win64-Shipping.exe".to_owned(),
            "RogueCore.exe".to_owned(),
        ],
    }
}

pub fn descriptor() -> GameDescriptor {
    let descriptor = GameDescriptor {
        schema_version: 1,
        game_id: ROGUE_CORE_GAME_ID.to_owned(),
        display_name: "Deep Rock Galactic: Rogue Core".to_owned(),
        adapter_version: ROGUE_CORE_MODULE_VERSION.to_owned(),
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
            variant(RELEASE_VARIANT_ID, "Steam Release", RELEASE_APP_ID),
            variant(PLAYTEST_VARIANT_ID, "Steam Playtest", PLAYTEST_APP_ID),
        ],
        input_formats: vec![
            GameInputFormat::Pak,
            GameInputFormat::Zip,
            GameInputFormat::DirectorySnapshot,
        ],
        source_bindings: vec![
            GameSourceBinding {
                source_id: "nexusmods".to_owned(),
                remote_game_key: "rogue-core".to_owned(),
                provider_game_id: "deeprockgalacticroguecore".to_owned(),
            },
            GameSourceBinding {
                source_id: "modcat".to_owned(),
                remote_game_key: "rogue-core".to_owned(),
                provider_game_id: "drgrc".to_owned(),
            },
            GameSourceBinding {
                source_id: "local".to_owned(),
                remote_game_key: "rogue-core".to_owned(),
                provider_game_id: "rogue-core".to_owned(),
            },
        ],
        resources: vec![
            GameResourceRequirement {
                resource_id: ROGUE_CORE_PATCH_RESOURCE_ID.to_owned(),
                version_range: ">=1,<2".to_owned(),
                optional: false,
                purpose: "signed Rogue Core integration content".to_owned(),
            },
            GameResourceRequirement {
                resource_id: UE4SS_RESOURCE_ID.to_owned(),
                version_range: ">=3,<4".to_owned(),
                optional: true,
                purpose: "UE4SS loader with Rogue Core UE 5.6 settings".to_owned(),
            },
        ],
        capabilities: vec![
            "discover".to_owned(),
            "validate-installation".to_owned(),
            "unreal-engine-5.6".to_owned(),
            "create-build-plan".to_owned(),
            "build-legacy-compatible-pak".to_owned(),
            "create-deploy-plan".to_owned(),
            "detect-legacy-install".to_owned(),
        ],
    };
    descriptor
        .validate()
        .expect("static Rogue Core descriptor must satisfy game contract");
    descriptor
}

pub fn module_manifest(target: &str, host_api: &str, worker: ModuleFile) -> ModuleManifest {
    ModuleManifest {
        schema_version: 1,
        id: ROGUE_CORE_MODULE_ID.to_owned(),
        version: ROGUE_CORE_MODULE_VERSION.to_owned(),
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
                scope: "selected:game:rogue-core".to_owned(),
            },
        ],
        contributes: json!({
            "games": [descriptor()],
            "scopeId": ROGUE_CORE_GAME_ID
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
