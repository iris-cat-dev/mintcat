use super::*;
type ClosureBindings<'a> = (BTreeMap<(String, String, u64), ScopedProviderBinding>, BTreeMap<String, &'a mintcat_runtime_release::PackageRecord>);


impl UpdatesCoordinator {
    fn closure_bindings<'a>(&self, environment: &'a EnvironmentSnapshot) -> Result<ClosureBindings<'a>> {
        let mut bindings = BTreeMap::new();
        let mut records = BTreeMap::new();
        for (scope, generation) in &environment.scopes {
            for record in generation.targets.signed.packages.values() { records.insert(record.digest.clone(), record); }
            for binding in &generation.targets.signed.release.provider_bindings {
                require(&binding.scope_id == scope, "signed provider scope mismatch")?;
                let record = generation.targets.signed.packages.get(&binding.provider_digest).ok_or_else(|| UpdateError::Blocked("signed provider package absent".into()))?;
                if !record.manifest.components.iter().any(|c| c.kind == "service" && c.state_model == "stateless") { continue; }
                require(record.manifest.components.iter().filter(|c| c.kind == "service").all(|c| c.runtime == "worker" && c.state_model == "stateless"), "private closure needs an actual stateless worker service")?;
                let key = (scope.clone(), binding.contract_id.clone(), binding.contract_version);
                require(bindings.insert(key, binding.clone()).is_none(), "ambiguous signed scoped provider")?;
            }
        }
        Ok((bindings, records))
    }
    pub(crate) async fn build_closure_supervisor(&self, environment: &EnvironmentSnapshot) -> Result<ModuleSupervisor> {
        let (bindings, records) = self.closure_bindings(environment)?;
        let digests: BTreeSet<_> = environment.scopes.values().flat_map(ScopeGeneration::digests).collect();
        let mut trust = mintcat_runtime_artifacts::TrustStore::default().restrict_to_modules(std::iter::empty())?;
        for generation in environment.scopes.values().filter(|generation| generation.targets.signed.packages.keys().any(|digest| digests.contains(digest))) {
            let root = mintcat_runtime_release::trust_pinned_root(generation.verified_root.clone(), 0)?;
            root.verify("targets", &generation.targets)?;
            for record in generation.targets.signed.packages.values().filter(|record| digests.contains(&record.digest)) {
                require(record.manifest.target == self.inner.policy.target && record.manifest.host_api == self.inner.policy.host_api, "historical worker target/API differs from this host")?;
                trust.extend_module_authority(root.artifact_trust_store()?.restrict_to_modules([(&record.manifest, &record.signature)])?)?;
            }
        }
        let artifacts = self.inner.artifacts.with_trust(trust);
        let supervisor = ModuleSupervisor::new(artifacts.clone(), id("closure"), &self.inner.layout.runtime_dir)?;
        for generation in environment.scopes.values().filter(|g| bindings.keys().any(|key| key.0 == g.targets.signed.release.scope_id)) {
            for required in &generation.targets.signed.release.singleton_requirements {
                let scope = required.binding_scope.as_deref().unwrap_or("platform");
                require(bindings.keys().any(|(bound_scope, contract, version)| bound_scope == scope && contract == &required.contract_id && mintcat_runtime_release::version_matches(&required.version_range, *version).unwrap_or(false)), "historical singleton needs its exact frozen worker binding")?;
            }
        }
        let mut pending = bindings.clone();
        let result: Result<()> = async {
            while !pending.is_empty() {
                let mut progress = false;
                for key in pending.keys().cloned().collect::<Vec<_>>() {
                    let binding = pending[&key].clone();
                    let record = records[&binding.provider_digest];
                    let ready = record.manifest.requires.iter().filter(|r| !r.optional).all(|r| {
                        let scope = match r.binding_scope.as_deref() {
                            None | Some("scope") => binding.scope_id.as_str(),
                            Some(scope) => scope,
                        };
                        bindings.keys().any(|k| k.0 == scope && k.1 == r.contract_id && mintcat_runtime_release::version_matches(&r.version_range, k.2).unwrap_or(false) && !pending.contains_key(k))
                    });
                    if !ready { continue; }
                    artifacts.load_module(&binding.provider_digest)?;
                    supervisor.activate(binding).await?;
                    pending.remove(&key); progress = true;
                }
                require(progress, "cyclic or unavailable immutable worker dependency closure")?;
            }
            for (scope, generation) in &environment.scopes {
                for record in generation.targets.signed.packages.values() {
                    if !bindings.values().any(|b| b.provider_digest == record.digest) { continue; }
                    if let Some(signed) = inventory::signed_game(record)? {
                        let contract = record.manifest.provides.iter().find(|p| p.contract_id == mintcat_game_api::GAME_ADAPTER_CONTRACT_ID).expect("signed_game verifies adapter");
                        let actual = supervisor.invoke(mintcat_runtime_supervisor::Invocation { scope_id: scope, contract_id: &contract.contract_id, contract_version: contract.version, method: "describe", params: Value::Null, grants: &[], deadline: std::time::Duration::from_secs(5) }).await?;
                        let expected: mintcat_game_api::GameDescriptor = serde_json::from_value(signed.descriptor)?;
                        let actual: mintcat_game_api::GameDescriptor = serde_json::from_value(actual)?;
                        actual.validate().map_err(|e| UpdateError::Blocked(format!("actual game descriptor invalid: {e}")))?;
                        require(serde_json::to_value(expected)? == serde_json::to_value(actual)?, "actual native game descriptor differs from signed scope inventory")?;
                    }
                }
            }
            Ok(())
        }.await;
        if let Err(error) = result { let _ = supervisor.shutdown_all().await; return Err(error); }
        Ok(supervisor)
    }
}
