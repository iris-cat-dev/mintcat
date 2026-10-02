use std::collections::BTreeMap;

use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore};
use mintcat_sdk::{ArtifactGrantDescriptor, WorkerBroker, WorkerBrokerRequest, WorkerRequest, WorkerRequestKind};
use serde::{de::DeserializeOwned, Serialize};

use crate::{
    worker_api::{WorkerEngineBuildRequest, WorkerEngineBuildResponse, WorkerEngineConflictsRequest,
        WorkerEngineInput, WorkerEngineVerifyRequest},
    FileConflict, InputInspection, UnrealBuildManifest, UnrealBuildRequest, UnrealBuildResult,
    UnrealEngineError, UnrealInput, UnrealPakEngine, VerificationReport,
    ENGINE_CONTRACT_ID, ENGINE_CONTRACT_VERSION,
};

/// The borrowed synchronous engine interface. Remote implementations must be used
/// from `spawn_blocking`, never directly on an asynchronous runtime thread.
pub trait UnrealEngineBackend: Clone {
    fn inspect(&self, input: UnrealInput<'_>) -> Result<InputInspection, UnrealEngineError>;
    fn conflicts(&self, inputs: Vec<UnrealInput<'_>>) -> Result<Vec<FileConflict>, UnrealEngineError>;
    fn build(&self, request: UnrealBuildRequest<'_>) -> Result<UnrealBuildResult, UnrealEngineError>;
    fn verify(&self, artifact: &ArtifactGrant, manifest: &UnrealBuildManifest)
        -> Result<VerificationReport, UnrealEngineError>;
}

impl UnrealEngineBackend for UnrealPakEngine {
    fn inspect(&self, input: UnrealInput<'_>) -> Result<InputInspection, UnrealEngineError> {
        UnrealPakEngine::inspect(self, input)
    }

    fn conflicts(&self, inputs: Vec<UnrealInput<'_>>) -> Result<Vec<FileConflict>, UnrealEngineError> {
        UnrealPakEngine::conflicts(self, inputs)
    }

    fn build(&self, request: UnrealBuildRequest<'_>) -> Result<UnrealBuildResult, UnrealEngineError> {
        UnrealPakEngine::build(self, request)
    }

    fn verify(&self, artifact: &ArtifactGrant, manifest: &UnrealBuildManifest)
        -> Result<VerificationReport, UnrealEngineError> {
        UnrealPakEngine::verify(self, artifact, manifest)
    }
}

/// Uses only the broker-selected exact engine package. There is no local fallback.
#[derive(Clone)]
pub struct RemoteUnrealPakEngine {
    broker: WorkerBroker,
    provider_digest: String,
    artifacts: ArtifactStore,
}

impl RemoteUnrealPakEngine {
    pub fn new(broker: WorkerBroker, provider_digest: String, artifacts: ArtifactStore) -> Self {
        Self { broker, provider_digest, artifacts }
    }

    fn invoke<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
        grants: impl IntoIterator<Item = ArtifactGrantDescriptor>,
    ) -> Result<R, UnrealEngineError> {
        let mut unique = BTreeMap::new();
        for grant in grants {
            match unique.entry(grant.grant_id.clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(grant);
                }
                std::collections::btree_map::Entry::Occupied(entry) if entry.get() != &grant => {
                    return Err(UnrealEngineError::InvalidRequest(
                        "conflicting engine input grant descriptors".to_owned(),
                    ));
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
            }
        }
        let produced: Vec<_> = unique.into_values().collect();
        // The host recognizes exact original descriptors; everything else must
        // be a verified worker-local output beneath the caller's workspace.
        let grant_ids = produced.iter().map(|grant| grant.grant_id.clone()).collect();
        let response = self.broker.blocking_invoke(WorkerBrokerRequest {
            id: String::new(),
            provider_digest: self.provider_digest.clone(),
            contract_id: ENGINE_CONTRACT_ID.to_owned(),
            contract_version: ENGINE_CONTRACT_VERSION,
            method: method.to_owned(),
            params: serde_json::to_value(params)?,
            grant_ids,
            produced,
        })?;
        Ok(serde_json::from_value(response)?)
    }
}

impl UnrealEngineBackend for RemoteUnrealPakEngine {
    fn inspect(&self, input: UnrealInput<'_>) -> Result<InputInspection, UnrealEngineError> {
        self.invoke("inspect", WorkerEngineInput::from_input(&input),
            [input.artifact.descriptor.clone()])
    }

    fn conflicts(&self, inputs: Vec<UnrealInput<'_>>) -> Result<Vec<FileConflict>, UnrealEngineError> {
        self.invoke("conflicts", WorkerEngineConflictsRequest {
            inputs: inputs.iter().map(WorkerEngineInput::from_input).collect(),
        }, inputs.iter().map(|input| input.artifact.descriptor.clone()))
    }

    fn build(&self, request: UnrealBuildRequest<'_>) -> Result<UnrealBuildResult, UnrealEngineError> {
        let response: WorkerEngineBuildResponse = self.invoke("build", WorkerEngineBuildRequest {
            plan_hash: request.plan_hash,
            inputs: request.inputs.iter().map(WorkerEngineInput::from_input).collect(),
            reference_paths: request.reference_paths,
            target: request.target,
        }, request.inputs.iter().map(|input| input.artifact.descriptor.clone()))?;
        if !response.result.artifact.verified
            || response.result.artifact.digest != response.grant.digest
            || response.result.artifact.size != response.grant.size
        {
            return Err(UnrealEngineError::Verification(
                "engine output does not match its authenticated grant".to_owned(),
            ));
        }
        // This descriptor arrives only via an authenticated broker response,
        // not from method parameters. Reuse the grant envelope validation.
        let envelope = WorkerRequest {
            id: String::new(),
            kind: WorkerRequestKind::Invoke,
            contract_id: Some(ENGINE_CONTRACT_ID.to_owned()),
            contract_version: Some(ENGINE_CONTRACT_VERSION),
            method: Some("build".to_owned()),
            params: serde_json::Value::Null,
            grants: vec![response.grant],
        };
        let grant = ArtifactGrant::from_worker_request(&envelope, &envelope.grants[0].grant_id)?;
        let adopted = self.artifacts.adopt_verified_grant(&grant)?;
        if adopted.digest != response.result.artifact.digest
            || adopted.size != response.result.artifact.size
        {
            return Err(UnrealEngineError::Verification(
                "adopted engine output does not match its result".to_owned(),
            ));
        }
        Ok(response.result)
    }

    fn verify(&self, artifact: &ArtifactGrant, manifest: &UnrealBuildManifest)
        -> Result<VerificationReport, UnrealEngineError> {
        self.invoke("verify", WorkerEngineVerifyRequest {
            grant_id: artifact.descriptor.grant_id.clone(),
            manifest: manifest.clone(),
        }, [artifact.descriptor.clone()])
    }
}
