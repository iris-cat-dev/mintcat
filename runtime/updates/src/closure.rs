use super::*;
type ClosureBindings<'a> = (BTreeMap<(String, String, u64), ScopedProviderBinding>, BTreeMap<String, &'a mintcat_runtime_release::PackageRecord>);

fn closure_digests(environment: &EnvironmentSnapshot, bindings: &BTreeMap<(String, String, u64), ScopedProviderBinding>, sources_only: bool) -> BTreeSet<String> {
    if !sources_only { return environment.scopes.values().flat_map(|generation| generation.targets.signed.packages.keys().cloned()).collect(); }
    let mut digests: BTreeSet<_> = bindings.values().map(|binding| binding.provider_digest.clone()).collect();
    loop {
        let previous = digests.len();
        for generation in environment.scopes.values() {
            for (owner, resources) in &generation.targets.signed.resource_dependencies {
                if digests.contains(owner) { digests.extend(resources.iter().cloned()); }
            }
        }
        if digests.len() == previous { return digests; }
    }
}

impl UpdatesCoordinator {
    /// A private, immutable worker realm for an admitted saved environment. The caller
    /// retains its durable source task lease and owns shutdown; preparing active scopes
    /// cannot drain it. Only source providers and their frozen dependencies launch,
    /// never unrelated games or duplicate stateful owners.
    pub async fn supervisor_for_environment(&self, environment: &EnvironmentSnapshot, source_network: Arc<dyn HostService>) -> Result<ModuleSupervisor> {
        self.authenticate_environment(environment)?;
        self.build_closure_supervisor(environment, true, Some(source_network)).await
    }
    pub fn source_closure_digests(&self, environment: &EnvironmentSnapshot) -> Result<Vec<String>> {
        self.authenticate_environment(environment)?;
        Ok(closure_digests(environment, &self.closure_bindings(environment, true)?.0, true).into_iter().collect())
    }
    fn closure_bindings<'a>(&self, environment: &'a EnvironmentSnapshot, sources_only: bool) -> Result<ClosureBindings<'a>> {
        let services = self.inner.host_services.lock().expect("host service registry poisoned").clone();
        let mut bindings = BTreeMap::new();
        let mut records = BTreeMap::new();
        for (scope, generation) in &environment.scopes {
            for record in generation.targets.signed.packages.values() { records.insert(record.digest.clone(), record); }
            for binding in &generation.targets.signed.release.provider_bindings {
                require(&binding.scope_id == scope, "signed provider scope mismatch")?;
                let record = generation.targets.signed.packages.get(&binding.provider_digest).ok_or_else(|| UpdateError::Blocked("signed provider package absent".into()))?;
                if !record.manifest.components.iter().any(|c| c.kind == "service" && c.state_model == "stateless") { continue; }
                require(record.manifest.components.iter().filter(|c| c.kind == "service").all(|c| c.runtime == "worker" && c.state_model == "stateless"), "private closure needs an actual stateless worker service")?;
                require(!(binding.contract_id == "source.network" && binding.contract_version == 1) && !services.contains_key(&(binding.contract_id.clone(), binding.contract_version)), "worker cannot shadow a host-owned contract")?;
                let key = (scope.clone(), binding.contract_id.clone(), binding.contract_version);
                require(bindings.insert(key, binding.clone()).is_none(), "ambiguous signed scoped provider")?;
            }
        }
        if sources_only {
            let mut reachable: BTreeSet<_> = bindings.iter().filter(|(_, b)| b.contract_id.strip_prefix("source.").is_some_and(|source| b.provider_module_id == format!("source-{source}"))).map(|(key, _)| key.clone()).collect();
            loop {
                let mut added = false;
                for key in reachable.iter().cloned().collect::<Vec<_>>() {
                    let binding = &bindings[&key];
                    for required in records[&binding.provider_digest].manifest.requires.iter().filter(|r| !r.optional) {
                        let scope = required.binding_scope.as_deref().unwrap_or(&binding.scope_id);
                        for dependency in bindings.keys().filter(|k| k.0 == scope && k.1 == required.contract_id && mintcat_runtime_release::version_matches(&required.version_range, k.2).unwrap_or(false)) {
                            added |= reachable.insert(dependency.clone());
                        }
                    }
                }
                if !added { break; }
            }
            bindings.retain(|key, _| reachable.contains(key));
            let state = self.read_state()?;
            for binding in bindings.values() { if state.revoked.contains(&binding.provider_digest) { return Err(UpdateError::ArtifactRevoked(binding.provider_digest.clone())); } }
        }
        Ok((bindings, records))
    }
    pub(crate) async fn build_closure_supervisor(&self, environment: &EnvironmentSnapshot, sources_only: bool, source_network: Option<Arc<dyn HostService>>) -> Result<ModuleSupervisor> {
        let (bindings, records) = self.closure_bindings(environment, sources_only)?;
        let mut services = self.inner.host_services.lock().expect("host service registry poisoned").clone();
        if let Some(service) = source_network { services.insert(("source.network".into(), 1), service); }
        let digests = closure_digests(environment, &bindings, sources_only);
        let mut trust = mintcat_runtime_artifacts::TrustStore::default().restrict_to_modules(std::iter::empty())?;
        for generation in environment.scopes.values().filter(|generation| generation.targets.signed.packages.keys().any(|digest| digests.contains(digest))) {
            let root = mintcat_runtime_release::trust_pinned_root(generation.verified_root.clone(), 0)?;
            root.verify("targets", &generation.targets)?;
            for record in generation.targets.signed.packages.values().filter(|record| digests.contains(&record.digest)) {
                self.inner.policy.validate(&record.manifest)?;
                trust.extend_module_authority(root.artifact_trust_store()?.restrict_to_modules([(&record.manifest, &record.signature)])?)?;
            }
        }
        let artifacts = self.inner.artifacts.with_trust(trust);
        let supervisor = ModuleSupervisor::new(artifacts.clone(), id("closure"), &self.inner.layout.runtime_dir)?;
        for ((contract, version), service) in &services { supervisor.register_host_service(contract, *version, service.clone())?; }
        for generation in environment.scopes.values().filter(|g| bindings.keys().any(|key| key.0 == g.targets.signed.release.scope_id)) {
            for required in &generation.targets.signed.release.singleton_requirements {
                let scope = required.binding_scope.as_deref().unwrap_or("platform");
                require(services.keys().any(|(contract, version)| contract == &required.contract_id && mintcat_runtime_release::version_matches(&required.version_range, *version).unwrap_or(false)) || bindings.keys().any(|(bound_scope, contract, version)| bound_scope == scope && contract == &required.contract_id && mintcat_runtime_release::version_matches(&required.version_range, *version).unwrap_or(false)), "historical singleton needs an exact frozen worker binding or approved actual host service")?;
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
                        let scope = r.binding_scope.as_deref().unwrap_or(&binding.scope_id);
                        services.keys().any(|(contract, version)| contract == &r.contract_id && mintcat_runtime_release::version_matches(&r.version_range, *version).unwrap_or(false)) || bindings.keys().any(|k| k.0 == scope && k.1 == r.contract_id && mintcat_runtime_release::version_matches(&r.version_range, k.2).unwrap_or(false) && !pending.contains_key(k))
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
