use std::{collections::BTreeMap, fs::File, path::{Path, PathBuf}, sync::Arc, time::Duration};

use mintcat_game_api::{
    DeployPrecondition, GameBuildPlan, GameDescriptor, LegacyInstallReport,
    ValidatedGameInstallation, WorkerGameBuildRequest, WorkerGameBuildResponse,
    WorkerGameDependency, WorkerGameDeployRequest, WorkerGamePreparationResponse, WorkerInputGrant,
    GAME_ADAPTER_CONTRACT_ID, GAME_ADAPTER_CONTRACT_VERSION,
};
use mintcat_runtime_artifacts::{ArtifactGrant, ArtifactStore, ModuleLease};
use mintcat_runtime_supervisor::{Invocation, ModuleSupervisor};
use mintcat_sdk::{ArtifactGrantDescriptor, ArtifactRef, ScopedProviderBinding};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sysinfo::{ProcessesToUpdate, System};
use tokio::runtime::Handle;

use crate::{AdapterBuildProduct, AdapterBuildRequest, BuildConflict, BuildPurpose, GameInstallAdapter, ModuleClosure};

/// One exact signed package closure. The supervisor binding is private to this
/// closure, so changing the active game scope cannot redirect an in-flight job.
pub struct WorkerGameAdapter {
    supervisor: ModuleSupervisor,
    artifacts: ArtifactStore,
    handle: Handle,
    scope: String,
    descriptor: GameDescriptor,
    closure: ModuleClosure,
    modules: Vec<ModuleLease>,
    dependencies: Vec<WorkerGameDependency>,
}

impl WorkerGameAdapter {
    pub async fn activate(
        supervisor: ModuleSupervisor,
        game_id: &str,
        game_package_digest: &str,
        closure: ModuleClosure,
    ) -> Result<Arc<Self>, String> {
        closure.verify().map_err(|error| error.to_string())?;
        let artifacts = supervisor.artifacts().clone();
        let mut modules = Vec::with_capacity(closure.entries.len());
        for entry in &closure.entries {
            let lease = artifacts.acquire_module(&entry.package_digest).map_err(|error| error.to_string())?;
            if lease.module().manifest.id != entry.module_id || lease.module().manifest.version != entry.version {
                return Err(format!("closure entry {} differs from its signed package", entry.module_id));
            }
            modules.push(lease);
        }
        let game = modules.iter().find(|lease| lease.module().digest == game_package_digest)
            .ok_or_else(|| "game package is absent from the module closure".to_owned())?;
        let descriptors: Vec<GameDescriptor> = serde_json::from_value(
            game.module().manifest.contributes.get("games").cloned()
                .ok_or_else(|| "signed game package has no games contribution".to_owned())?,
        ).map_err(|error| error.to_string())?;
        let descriptor = descriptors.into_iter().find(|descriptor| descriptor.game_id == game_id)
            .ok_or_else(|| format!("signed game package does not contribute {game_id}"))?;
        descriptor.validate().map_err(|error| error.to_string())?;
        let scope = format!("{game_id}@{}", closure.digest);
        let mut dependencies = Vec::with_capacity(game.module().manifest.requires.len());
        for requirement in &game.module().manifest.requires {
            if requirement.binding_scope.as_deref() == Some("host") { continue; }
            let range = requirement.version_range.trim();
            let mut selected = None;
            for provider in &modules {
                for contract in &provider.module().manifest.provides {
                    if contract.contract_id != requirement.contract_id
                        || !(range == "*" || range.strip_prefix('=').unwrap_or(range)
                            .parse::<u64>().is_ok_and(|version| version == contract.version)) {
                        continue;
                    }
                    if selected.is_some() {
                        return Err(format!("game closure has multiple providers for {}@{}",
                            requirement.contract_id, requirement.version_range));
                    }
                    selected = Some((provider, contract));
                }
            }
            let Some((provider, contract)) = selected else {
                if requirement.optional { continue; }
                return Err(format!("game closure has no declared provider for {}@{}",
                    requirement.contract_id, requirement.version_range));
            };
            supervisor.activate(ScopedProviderBinding {
                scope_id: scope.clone(), contract_id: contract.contract_id.clone(),
                contract_version: contract.version,
                provider_module_id: provider.module().manifest.id.clone(),
                provider_digest: provider.module().digest.clone(),
            }).await.map_err(|error| error.to_string())?;
            dependencies.push(WorkerGameDependency {
                contract_id: contract.contract_id.clone(), contract_version: contract.version,
                package_digest: provider.module().digest.clone(),
            });
        }
        supervisor.activate(ScopedProviderBinding {
            scope_id: scope.clone(), contract_id: GAME_ADAPTER_CONTRACT_ID.to_owned(),
            contract_version: GAME_ADAPTER_CONTRACT_VERSION,
            provider_module_id: game.module().manifest.id.clone(),
            provider_digest: game.module().digest.clone(),
        }).await.map_err(|error| error.to_string())?;
        Ok(Arc::new(Self {
            supervisor, artifacts, handle: Handle::current(), scope, descriptor, closure,
            modules, dependencies,
        }))
    }

    pub fn descriptor(&self) -> &GameDescriptor { &self.descriptor }
    pub fn closure(&self) -> &ModuleClosure { &self.closure }
    /// Hints are supplied by the trusted host, not interpreted as game-specific
    /// discovery rules. Only the active signed native game inspects them.
    pub fn discover_installations(
        &self,
        roots: &[(PathBuf, String)],
        include_steam: bool,
    ) -> Result<Vec<mintcat_game_api::GameInstallationCandidate>, String> {
        if roots.len() > 32 || roots.iter().any(|(root, variant)| !root.is_absolute()
            || root.as_os_str().len() > 4096
            || !self.descriptor.variants.iter().any(|v| v.variant_id == *variant)) {
            return Err("discovery hints must be at most 32 bounded absolute roots with signed variants".into());
        }
        let candidates: Vec<mintcat_game_api::GameInstallationCandidate> = self.invoke(
            "installation.discover",
            json!({"roots":roots.iter().map(|(root, variant)| json!({"rootPath":root,"variantId":variant})).collect::<Vec<_>>(),"includeSteam":include_steam}),
            &[],
        )?;
        if candidates.iter().any(|candidate| candidate.game_id != self.descriptor.game_id
            || !self.descriptor.variants.iter().any(|v| v.variant_id == candidate.variant_id)
            || !Path::new(&candidate.root_path).is_absolute()) {
            return Err("native discovery returned an installation outside the signed game scope".into());
        }
        Ok(candidates)
    }

    pub fn validate_selected_installation(
        &self,
        root: &Path,
        variant_id: &str,
    ) -> Result<ValidatedGameInstallation, String> {
        <Self as GameInstallAdapter>::validate_installation(self, root, variant_id)
    }
    pub fn settings_snapshot(
        &self,
        revision: u64,
        options: Value,
    ) -> Result<mintcat_game_api::GameSettingsSnapshot, String> {
        self.invoke("settings.snapshot", json!({"revision":revision,"options":options}), &[])
    }

    fn invoke<T: DeserializeOwned>(&self, method: &str, params: Value, grants: &[ArtifactGrant]) -> Result<T, String> {
        let value = self.handle.block_on(self.supervisor.invoke(Invocation {
            scope_id: &self.scope, contract_id: GAME_ADAPTER_CONTRACT_ID,
            contract_version: GAME_ADAPTER_CONTRACT_VERSION, method, params, grants,
            deadline: Duration::from_secs(60 * 30),
        })).map_err(|error| error.to_string())?;
        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    fn patch_package(&self) -> Result<Option<&ModuleLease>, String> {
        let mut required = self.descriptor.resources.iter().filter(|resource| !resource.optional);
        let Some(requirement) = required.next() else { return Ok(None); };
        if required.next().is_some() {
            return Err("game adapter must identify one required patch resource".to_owned());
        }
        let package = self.modules.iter().find(|package| package.module().manifest.id == requirement.resource_id);
        if let Some(package) = package {
            let version = semver::Version::parse(&package.module().manifest.version).map_err(|error| error.to_string())?;
            let range = semver::VersionReq::parse(&requirement.version_range).map_err(|error| error.to_string())?;
            if !range.matches(&version) { return Err("signed patch version is outside the game descriptor range".to_owned()); }
        }
        Ok(package)
    }

    fn adopt_output(&self, artifact: &ArtifactRef, descriptor: &ArtifactGrantDescriptor) -> Result<(), String> {
        if !artifact.verified || descriptor.digest != artifact.digest || descriptor.size != artifact.size {
            return Err("worker output descriptor differs from the built artifact".to_owned());
        }
        let grant = self.supervisor.grant_output(&self.scope, GAME_ADAPTER_CONTRACT_ID,
            GAME_ADAPTER_CONTRACT_VERSION, descriptor).map_err(|error| error.to_string())?;
        let published = self.artifacts.adopt_verified_grant(&grant).map_err(|error| error.to_string())?;
        if published.digest != artifact.digest || published.size != artifact.size || !published.verified {
            return Err("worker output changed before host publication".to_owned());
        }
        Ok(())
    }
}

impl GameInstallAdapter for WorkerGameAdapter {
    fn game_id(&self) -> &str { &self.descriptor.game_id }

    fn validate_installation(&self, root: &Path, variant_id: &str) -> Result<ValidatedGameInstallation, String> {
        self.ensure_stopped(root)?;
        let variant = self.descriptor.variants.iter().find(|variant| variant.variant_id == variant_id)
            .ok_or_else(|| "selected variant is absent from the signed descriptor".to_owned())?;
        let installation: ValidatedGameInstallation = self.invoke("installation.validate", json!({"rootPath":root,"variantId":variant_id}), &[])?;
        if installation.game_id != self.descriptor.game_id || installation.variant_id != variant_id
            || installation.main_pak_relative_path != variant.main_pak_relative_path
            || installation.target != variant.target
            || root.canonicalize().map_err(|error| error.to_string())? != Path::new(&installation.root_path) {
            return Err("native validation returned an installation outside the signed game scope".into());
        }
        Ok(installation)
    }

    fn ensure_stopped(&self, _root: &Path) -> Result<(), String> {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);
        for process in system.processes().values() {
            if self.descriptor.variants.iter().flat_map(|variant| &variant.process_names)
                .any(|name| process.name().to_string_lossy().eq_ignore_ascii_case(name)) {
                return Err(format!("game process {} ({}) is running", process.name().to_string_lossy(), process.pid()));
            }
        }
        Ok(())
    }

    fn detect_legacy_install(&self, root: &Path, variant_id: &str) -> Result<LegacyInstallReport, String> {
        self.invoke("legacy.detect", json!({"rootPath":root,"variantId":variant_id}), &[])
    }

    fn build(&self, request: AdapterBuildRequest<'_>) -> Result<AdapterBuildProduct, String> {
        if request.module_closure != &self.closure {
            return Err("requested module closure differs from the pinned worker closure".to_owned());
        }
        if request.cancellation.is_requested() { return Err("build was cancelled".to_owned()); }
        let root = Path::new(&request.installation.root_path);
        let base = self.artifacts.publish_blob_file(&root.join(&request.installation.main_pak_relative_path),
            None).map_err(|error| error.to_string())?;
        if base.digest != request.installation.fingerprint {
            return Err("base game changed during artifact import".to_owned());
        }
        let patch = self.patch_package()?;
        if request.purpose == BuildPurpose::Install && patch.is_none()
            && self.descriptor.resources.iter().any(|resource| !resource.optional) {
            return Err("install requires the signed patch package in the module closure".to_owned());
        }
        let build_plan: GameBuildPlan = self.invoke("build.plan", json!({
            "installation":request.installation, "profileRevision":request.profile.profile_revision,
            "moduleLockDigest":self.closure.digest, "settings":request.settings,
            "baseGameArtifact":base, "patchResourceLockDigest":patch.map(|patch| &patch.module().digest),
            "orderedInputs":request.profile.planned_inputs(),
        }), &[])?;
        let mut grants = Vec::with_capacity(request.input_grants.len() + 1);
        grants.push(self.artifacts.grant_blob(&base).map_err(|error| error.to_string())?);
        for input in &request.profile.ordered_inputs {
            grants.push(self.artifacts.grant_blob(&input.input.artifact).map_err(|error| error.to_string())?);
        }
        let patch_package = patch.map(|package| self.artifacts.module_worker_grant(package)).transpose()
            .map_err(|error| error.to_string())?;
        let patch_archive_path = patch.map(|package| {
            if package.module().manifest.files.len() != 1 {
                return Err("signed patch package must contain exactly one archive".to_owned());
            }
            Ok(package.module().manifest.files[0].path.clone())
        }).transpose()?;
        let resources = self.modules.iter().filter(|package| self.descriptor.resources.iter()
            .any(|resource| resource.optional && resource.resource_id == package.module().manifest.id))
            .map(|package| self.artifacts.module_worker_grant(package))
            .collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
        let params = serde_json::to_value(WorkerGameBuildRequest {
            plan: build_plan.clone(), base_grant_id: grants[0].descriptor.grant_id.clone(),
            dependencies: self.dependencies.clone(),
            inputs: request.profile.ordered_inputs.iter().zip(&grants[1..]).map(|(input, grant)| WorkerInputGrant {
                input_id: input.profile_mod_id.clone(), grant_id: grant.descriptor.grant_id.clone(),
                mod_id: input.mod_id.clone(),
            }).collect(), patch_package, patch_archive_path, resources,
        }).map_err(|error| error.to_string())?;
        let preparation: WorkerGamePreparationResponse = self.invoke("build.prepare", params.clone(), &grants)?;
        let prepared_grants: BTreeMap<_, _> = preparation.grants.iter()
            .map(|grant| (grant.digest.as_str(), grant)).collect();
        for file in preparation.companion_manifests.iter().flat_map(|manifest| &manifest.files) {
            let grant = prepared_grants.get(file.artifact.digest.as_str())
                .ok_or_else(|| "prepared deployment artifact has no authenticated grant".to_owned())?;
            self.adopt_output(&file.artifact, grant)?;
        }
        let companion_manifests = preparation.companion_manifests;
        let conflicts = preparation.conflicts.into_iter().map(|conflict| BuildConflict {
            relative_path: conflict.relative_path, winner_input_id: conflict.winner_input_id,
            shadowed_input_ids: conflict.shadowed_input_ids,
        }).collect();
        let exported: WorkerGameBuildResponse = self.invoke("build.execute", params, &grants)?;
        let mut outputs = Vec::with_capacity(exported.outputs.len());
        for exported in exported.outputs {
            self.adopt_output(&exported.output.artifact, &exported.grant)?;
            outputs.push(exported.output);
        }
        let deploy_plan = if request.purpose == BuildPurpose::Install {
            let mut preconditions = BTreeMap::new();
            for target in outputs.iter().map(|output| &output.target_relative_path)
                .chain(companion_manifests.iter().flat_map(|manifest| manifest.files.iter().map(|file| &file.target_relative_path))) {
                let path = root.join(target);
                let expected = match File::open(&path) {
                    Ok(mut file) => {
                        let artifact = self.artifacts.publish_blob_reader(&mut file, None).map_err(|error| error.to_string())?;
                        DeployPrecondition::Digest(artifact.digest)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => DeployPrecondition::Absent,
                    Err(error) => return Err(format!("reading target {target}: {error}")),
                };
                preconditions.insert(target.to_ascii_lowercase(), expected);
            }
            Some(self.invoke("deploy.plan", serde_json::to_value(WorkerGameDeployRequest {
                build_plan: build_plan.clone(), built_outputs: outputs.clone(),
                loader_manifests: companion_manifests.clone(), preconditions,
            }).map_err(|error| error.to_string())?, &[])? )
        } else { None };
        Ok(AdapterBuildProduct { build_plan, outputs, deploy_plan, conflicts, companion_manifests })
    }
}
