use mintcat_sdk::{
    ModuleActivation, ModuleComponent, ModuleFile, ModuleManifest, ModuleState,
    PermissionRequest, ProvidedContract,
};
use serde_json::json;

use crate::{ENGINE_CONTRACT_ID, ENGINE_CONTRACT_VERSION, ENGINE_MODULE_ID, ENGINE_MODULE_VERSION};

/// Manifest for the independently signed native engine package.
pub fn module_manifest(target: &str, host_api: &str, worker: ModuleFile) -> ModuleManifest {
    ModuleManifest {
        schema_version: 1,
        id: ENGINE_MODULE_ID.to_owned(),
        version: ENGINE_MODULE_VERSION.to_owned(),
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
            contract_id: ENGINE_CONTRACT_ID.to_owned(),
            version: ENGINE_CONTRACT_VERSION,
            cardinality: Some("multi".to_owned()),
        }],
        requires: Vec::new(),
        permissions: vec![
            PermissionRequest {
                capability: "artifact.read".to_owned(),
                scope: "operation-grants".to_owned(),
            },
            PermissionRequest {
                capability: "staging.write".to_owned(),
                scope: "operation-staging".to_owned(),
            },
        ],
        contributes: json!({}),
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
