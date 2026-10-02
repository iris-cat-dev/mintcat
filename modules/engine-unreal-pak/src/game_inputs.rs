//! Unreal input and UE4SS planning called explicitly by Unreal game workers.
use std::collections::BTreeMap;

use mintcat_game_api::{
    GameContractError, GameDescriptor, GameInputFormat, WorkerGameBuildRequest, WorkerGameConflict,
    WorkerGameDependency, WorkerGamePreparationResponse,
};
use mintcat_loader_ue4ss::{
    JsDeploymentRequest, JsInputFormat, LoaderLimits, ResourceDeploymentRequest,
    ResourceFileBinding, Ue4ssLoader, Ue4ssSettings,
};
use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore, ModuleLease};
use mintcat_sdk::WorkerRequest;
use serde::Deserialize;

use crate::{
    UnrealEngineBackend, UnrealInput, UnrealInputFormat, ENGINE_CONTRACT_ID,
    ENGINE_CONTRACT_VERSION,
};

pub fn selected_provider(dependencies: &[WorkerGameDependency]) -> Result<&str, GameContractError> {
    dependencies
        .iter()
        .find(|dependency| {
            dependency.contract_id == ENGINE_CONTRACT_ID
                && dependency.contract_version == ENGINE_CONTRACT_VERSION
        })
        .map(|dependency| dependency.package_digest.as_str())
        .ok_or_else(|| {
            GameContractError::Invalid(
                "game request omits its declared Unreal engine provider".to_owned(),
            )
        })
}

pub fn prepare<E: UnrealEngineBackend>(
    request: &WorkerRequest,
    params: WorkerGameBuildRequest,
    descriptor: &GameDescriptor,
    settings: Ue4ssSettings,
    engine: &E,
    artifacts: &ArtifactStore,
) -> Result<WorkerGamePreparationResponse, GameContractError> {
    let invalid = |error: &dyn std::fmt::Display| GameContractError::Invalid(error.to_string());
    let loader_enabled = params
        .plan
        .settings
        .options
        .get("ue4ssEnabled")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            GameContractError::Invalid("game settings do not declare ue4ssEnabled".to_owned())
        })?;
    let loader = Ue4ssLoader::new(artifacts.clone(), LoaderLimits::default())
        .map_err(|error| invalid(&error))?;
    let inputs = params
        .inputs
        .iter()
        .map(|input| {
            let grant = ArtifactGrant::from_worker_request(request, &input.grant_id)
                .map_err(|error| invalid(&error))?;
            Ok((input.input_id.as_str(), (input, grant)))
        })
        .collect::<Result<BTreeMap<_, _>, GameContractError>>()?;
    let mut paks = Vec::new();
    let mut companion_manifests = Vec::new();
    for input in &params.plan.ordered_inputs {
        if input.audio_only_hint {
            continue;
        }
        let (binding, grant) = inputs.get(input.input_id.as_str()).ok_or_else(|| {
            GameContractError::Invalid(format!("input {} has no authorized grant", input.input_id))
        })?;
        let format = match input.format {
            GameInputFormat::Pak => UnrealInputFormat::Pak,
            GameInputFormat::Zip => UnrealInputFormat::Zip,
            GameInputFormat::DirectorySnapshot => UnrealInputFormat::DirectorySnapshot,
        };
        let unreal = || UnrealInput {
            input_id: input.input_id.clone(),
            artifact: grant,
            format,
            content_prefix: None,
        };
        match engine.inspect(unreal()) {
            Ok(_) => paks.push(unreal()),
            Err(pak_error) => {
                let format = match input.format {
                    GameInputFormat::Pak => return Err(invalid(&pak_error)),
                    GameInputFormat::Zip => JsInputFormat::Zip,
                    GameInputFormat::DirectorySnapshot => JsInputFormat::DirectorySnapshot,
                };
                let manifest = loader
                    .plan_js_mod(JsDeploymentRequest {
                        game_id: descriptor.game_id.clone(),
                        owner: format!("{}/loader/{}", descriptor.game_id, input.input_id),
                        mod_id: binding.mod_id.replace(':', "-"),
                        input_id: input.input_id.clone(),
                        artifact: grant,
                        format,
                    })
                    .map_err(|loader_error| {
                        GameContractError::Invalid(format!(
                            "invalid game input: {pak_error}; {loader_error}"
                        ))
                    })?;
                if !loader_enabled {
                    return Err(GameContractError::Invalid(format!(
                        "input {} requires UE4SS, but it is disabled",
                        input.input_id,
                    )));
                }
                companion_manifests.push(manifest);
            }
        }
    }
    if loader_enabled {
        let grant = params
            .resources
            .into_iter()
            .find(|package| {
                descriptor.resources.iter().any(|requirement| {
                    requirement.optional && requirement.resource_id == package.manifest.id
                })
            })
            .ok_or_else(|| {
                GameContractError::Invalid(
                    "UE4SS is enabled but its signed resource is absent from the closure"
                        .to_owned(),
                )
            })?;
        let package = ModuleLease::from_worker_grant(grant).map_err(|error| invalid(&error))?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Binding {
            package_path: String,
            target_relative_path: String,
            media_type: Option<String>,
        }
        let bindings: Vec<Binding> = serde_json::from_value(
            package
                .module()
                .manifest
                .contributes
                .get("deploymentBindings")
                .cloned()
                .ok_or_else(|| {
                    GameContractError::Invalid(
                        "signed UE4SS resource omits exact deploymentBindings".to_owned(),
                    )
                })?,
        )?;
        companion_manifests.insert(
            0,
            loader
                .plan_resource_package(ResourceDeploymentRequest {
                    game_id: descriptor.game_id.clone(),
                    owner: format!("{}/loader-resource", descriptor.game_id),
                    package: &package,
                    files: bindings
                        .into_iter()
                        .map(|binding| ResourceFileBinding {
                            package_path: binding.package_path,
                            target_relative_path: binding.target_relative_path,
                            media_type: binding.media_type,
                        })
                        .collect(),
                    settings,
                })
                .map_err(|error| invalid(&error))?,
        );
    }
    let variant = descriptor
        .variants
        .iter()
        .find(|variant| variant.variant_id == params.plan.variant_id)
        .ok_or_else(|| {
            GameContractError::Invalid("variant is absent from the signed descriptor".to_owned())
        })?;
    let mut grants = BTreeMap::new();
    for manifest in &mut companion_manifests {
        for file in &mut manifest.files {
            file.target_relative_path = format!(
                "{}/{}",
                variant.binaries_relative_path, file.target_relative_path
            );
            if !grants.contains_key(&file.artifact.digest) {
                let grant = artifacts
                    .grant_blob(&file.artifact)
                    .map_err(|error| invalid(&error))?;
                grants.insert(file.artifact.digest.clone(), grant.descriptor);
            }
        }
        for approval in &mut manifest.approval_requirements {
            approval.target_relative_path = format!(
                "{}/{}",
                variant.binaries_relative_path, approval.target_relative_path
            );
        }
    }
    let conflicts = engine
        .conflicts(paks)
        .map_err(|error| invalid(&error))?
        .into_iter()
        .map(|conflict| WorkerGameConflict {
            relative_path: conflict.relative_path,
            winner_input_id: conflict.winner_input_id,
            shadowed_input_ids: conflict.shadowed_input_ids,
        })
        .collect();
    Ok(WorkerGamePreparationResponse {
        conflicts,
        companion_manifests,
        grants: grants.into_values().collect(),
    })
}
