use super::*;

impl UpdatesCoordinator {
    pub(crate) fn generation_store(&self, generation: &ScopeGeneration) -> Result<ArtifactStore> {
        // The root was captured from RepositoryVerifier's proof at admission and committed
        // in the kernel owner database. It is not taken from arbitrary package/network input.
        let root = mintcat_runtime_release::trust_pinned_root(generation.verified_root.clone(), 0)?;
        root.verify("targets", &generation.targets)?;
        let trust = root.artifact_trust_store()?.restrict_to_modules(generation.targets.signed.packages.values().map(|record| (&record.manifest, &record.signature)))?;
        Ok(self.inner.artifacts.with_trust(trust))
    }
    pub fn artifact_store_for_generation(&self, scope: &str, generation_id: &str) -> Result<ArtifactStore> {
        let state = self.read_state()?;
        let generation = state.environment.scopes.get(scope).filter(|g| g.generation_id == generation_id)
            .or_else(|| state.operations.values().filter(|op| op.scope_id == scope).find_map(|op| {
                if op.generation.generation_id == generation_id && op.generation.confirmed { Some(&op.generation) }
                else { op.previous.as_ref().filter(|g| g.generation_id == generation_id && g.confirmed) }
            })).ok_or_else(|| UpdateError::Blocked("artifact authority requires an active probation/confirmed or historical confirmed scoped generation".into()))?;
        self.generation_store(generation)
    }
    pub fn artifact_store_for_digest(&self, digest: &str) -> Result<ArtifactStore> {
        let state = self.read_state()?;
        let generation = state.environment.scopes.values().find(|g| g.targets.signed.packages.contains_key(digest))
            .or_else(|| state.operations.values().find_map(|op| {
                if op.generation.confirmed && op.generation.targets.signed.packages.contains_key(digest) { Some(&op.generation) }
                else { op.previous.as_ref().filter(|g| g.confirmed && g.targets.signed.packages.contains_key(digest)) }
            })).ok_or_else(|| UpdateError::Blocked(format!("digest {digest} has no admitted authenticated scope provenance")))?;
        self.generation_store(generation)
    }
    /// Exact protected receipts only: publisher keys from unrelated historical roots
    /// never become generally trusted for this mixed immutable closure.
    pub fn artifact_store_for_digests(&self, digests: &[String]) -> Result<ArtifactStore> {
        let state = self.read_state()?;
        let mut trust = mintcat_runtime_artifacts::TrustStore::default().restrict_to_modules(std::iter::empty())?;
        for digest in digests.iter().collect::<BTreeSet<_>>() {
            let generation = known_generations(&state).find(|g| g.targets.signed.packages.contains_key(digest))
                .ok_or_else(|| UpdateError::Blocked(format!("digest {digest} has no admitted authenticated scope provenance")))?;
            let root = mintcat_runtime_release::trust_pinned_root(generation.verified_root.clone(), 0)?;
            root.verify("targets", &generation.targets)?;
            let record = &generation.targets.signed.packages[digest];
            self.inner.policy.validate(&record.manifest)?;
            root.verify_package_signature(&record.manifest, &record.signature, &mintcat_runtime_artifacts::module_manifest_signing_digest(&record.manifest)?)?;
            trust.extend_module_authority(root.artifact_trust_store()?.restrict_to_modules([(&record.manifest, &record.signature)])?)?;
        }
        Ok(self.inner.artifacts.with_trust(trust))
    }
    pub(crate) fn authenticate_environment(&self, environment: &EnvironmentSnapshot) -> Result<()> {
        let state = self.read_state()?;
        for (scope, generation) in &environment.scopes {
            require(scope == &generation.targets.signed.release.scope_id, "environment scope identity mismatch")?;
            let committed = known_generations(&state).find(|g| g.generation_id == generation.generation_id && g.targets.signed.release.scope_id == *scope)
                .ok_or_else(|| UpdateError::Blocked("environment contains an unadmitted generation".into()))?;
            require(serde_json::to_value(&committed.targets)? == serde_json::to_value(&generation.targets)? && serde_json::to_value(&committed.verified_root)? == serde_json::to_value(&generation.verified_root)?,
                "environment changed the protected authenticated generation proof")?;
        }
        Ok(())
    }
}

fn known_generations(state: &UpdateState) -> impl Iterator<Item = &ScopeGeneration> {
    state.environment.scopes.values().chain(state.operations.values().flat_map(|op| {
        op.generation.confirmed.then_some(&op.generation).into_iter().chain(op.previous.as_ref().filter(|g| g.confirmed))
    }))
}
