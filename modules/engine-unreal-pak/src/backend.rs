use mintcat_runtime_artifacts::ArtifactGrant;

use crate::{FileConflict, InputInspection, UnrealBuildManifest, UnrealBuildRequest, UnrealBuildResult,
    UnrealEngineError, UnrealInput, UnrealPakEngine, VerificationReport};

/// The borrowed synchronous backend shared statically by each game worker.
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

