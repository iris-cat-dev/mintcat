use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};
use mintcat_sdk::{ReleasePackage, RequiredContract, ScopeReleaseSet, ScopedProviderBinding};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::{canonical, require, sha256, version_matches, MetadataHeader, PackageRecord, Result, Signed, TrustedRoot, VerificationPolicy};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CohortPolicy {
    pub salt: String,
    pub basis_points: u16,
}
impl CohortPolicy {
    pub fn includes(&self, scope: &str, channel: &str, installation_id: &str) -> Result<bool> {
        self.validate()?;
        require(!installation_id.is_empty(), "cohort requires a stable installation identity")?;
        let bytes = canonical(&(scope, channel, &self.salt, installation_id))?;
        let digest = Sha256::digest(bytes);
        let bucket = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 width")) % 10000;
        Ok(bucket < self.basis_points as u64)
    }
    fn validate(&self) -> Result<()> { require(!self.salt.is_empty() && self.basis_points <= 10000, "invalid cohort policy") }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScopePlan {
    pub scope_id: String,
    pub release_id: String,
    pub sequence: u64,
    pub channel: String,
    pub target: String,
    pub min_host_api: String,
    pub roots: Vec<String>,
    pub provider_bindings: Vec<ScopedProviderBinding>,
    pub singleton_requirements: Vec<RequiredContract>,
    /// Exact digest edges for resources/native payloads without a contract provider.
    pub resource_dependencies: BTreeMap<String, Vec<String>>,
    pub cohort: CohortPolicy,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetsMetadata {
    pub header: MetadataHeader,
    pub release: ScopeReleaseSet,
    pub cohort: CohortPolicy,
    pub roots: Vec<String>,
    pub resource_dependencies: BTreeMap<String, Vec<String>>,
    pub packages: BTreeMap<String, PackageRecord>,
}
#[derive(Debug, Clone)]
pub struct VerifiedScopeRelease {
    targets: Signed<TargetsMetadata>,
    root: Arc<TrustedRoot>,
    metadata_expires_at: u64,
}
impl VerifiedScopeRelease {
    pub fn release(&self) -> &ScopeReleaseSet { &self.targets.signed.release }
    pub fn targets(&self) -> &Signed<TargetsMetadata> { &self.targets }
    pub fn package_records(&self) -> &BTreeMap<String, PackageRecord> { &self.targets.signed.packages }
    pub fn cohort(&self) -> &CohortPolicy { &self.targets.signed.cohort }
    pub fn verified_root(&self) -> &TrustedRoot { self.root.as_ref() }
    pub fn metadata_expires_at(&self) -> u64 { self.metadata_expires_at }
    /// Required before using a prepared proof to start a new scope activation.
    /// Already-confirmed offline closures use their separate committed authority.
    pub fn require_fresh(&self, now: u64) -> Result<()> {
        require(now < self.metadata_expires_at, "prepared release metadata has expired")
    }
    pub(crate) fn from_verified(targets: Signed<TargetsMetadata>, root: Arc<TrustedRoot>, metadata_expires_at: u64) -> Self { Self { targets, root, metadata_expires_at } }
}
pub fn scope_metadata_key(scope: &str, channel: &str, target: &str) -> Result<String> {
    Ok(sha256(&canonical(&(scope, channel, target))?)[7..].to_owned())
}

pub fn compose_scope(plan: ScopePlan, available: Vec<PackageRecord>, policy: &VerificationPolicy, expires: u64) -> Result<TargetsMetadata> {
    require(!plan.scope_id.is_empty() && !plan.release_id.is_empty() && !plan.channel.is_empty() && plan.sequence > 0, "invalid scope release identity")?;
    require(plan.target == policy.target && host_satisfies_minimum(&policy.host_api, &plan.min_host_api), "scope target mismatch or minimum host API not met")?;
    let mut packages = BTreeMap::new();
    let mut identities = BTreeSet::new();
    for package in available {
        require(identities.insert((package.manifest.id.clone(), package.manifest.version.clone(), package.manifest.target.clone())), "duplicate package identity")?;
        require(packages.insert(package.digest.clone(), package).is_none(), "duplicate package digest")?;
    }
    let release = ScopeReleaseSet {
        schema_version: 1, scope_id: plan.scope_id, release_id: plan.release_id,
        sequence: plan.sequence, channel: plan.channel, target: plan.target, min_host_api: plan.min_host_api,
        packages: Vec::new(), provider_bindings: plan.provider_bindings,
        singleton_requirements: plan.singleton_requirements, signature_metadata_ref: String::new(),
    };
    let mut targets = TargetsMetadata {
        header: MetadataHeader { kind: "targets".into(), schema_version: 1, version: release.sequence, expires },
        release, cohort: plan.cohort, roots: plan.roots,
        resource_dependencies: plan.resource_dependencies, packages,
    };
    targets.release.signature_metadata_ref = scope_metadata_key(&targets.release.scope_id, &targets.release.channel, &targets.release.target)?;
    let closure = dependency_closure(&targets, policy)?;
    targets.packages.retain(|digest, _| closure.contains(digest));
    targets.resource_dependencies.retain(|digest, _| closure.contains(digest));
    targets.release.packages = targets.packages.values().map(|p| ReleasePackage {
        module_id: p.manifest.id.clone(), version: p.manifest.version.clone(), target: p.manifest.target.clone(), digest: p.digest.clone(), schema_version: p.manifest.schema_version,
    }).collect();
    // Reject unused binding declarations instead of signing an ambiguous graph.
    validate_targets(&targets, policy)?;
    Ok(targets)
}

pub(crate) fn validate_targets(targets: &TargetsMetadata, policy: &VerificationPolicy) -> Result<()> {
    let release = &targets.release;
    require(release.schema_version == 1 && !release.scope_id.is_empty() && !release.release_id.is_empty() && !release.channel.is_empty() && release.sequence > 0, "invalid scope release")?;
    require(targets.header.version == release.sequence, "scope metadata version must equal sequence")?;
    require(release.target == policy.target && host_satisfies_minimum(&policy.host_api, &release.min_host_api), "scope target mismatch or minimum host API not met")?;
    require(release.signature_metadata_ref == scope_metadata_key(&release.scope_id, &release.channel, &release.target)?, "scope metadata reference mismatch")?;
    targets.cohort.validate()?;
    let closure = dependency_closure(targets, policy)?;
    require(closure == targets.packages.keys().cloned().collect(), "release includes unrelated or missing packages")?;
    let mut identities = BTreeSet::new();
    let mut expected = Vec::new();
    for (digest, package) in &targets.packages {
        require(*digest == package.digest, "package digest index mismatch")?;
        policy.validate(&package.manifest)?;
        require(identities.insert((&package.manifest.id, &package.manifest.target)), "scope cannot contain two versions of the same module")?;
        if let Some(scope) = package.manifest.contributes.get("scopeId").and_then(serde_json::Value::as_str) {
            require(scope == release.scope_id, "package belongs to an unrelated scope")?;
        }
        expected.push(ReleasePackage { module_id: package.manifest.id.clone(), version: package.manifest.version.clone(), target: package.manifest.target.clone(), digest: package.digest.clone(), schema_version: package.manifest.schema_version });
        if targets.roots.contains(digest) && package.manifest.components.iter().any(|component| component.kind == "service" && component.state_model == "stateless") {
            require(release.provider_bindings.iter().any(|binding| binding.provider_digest == *digest), "root worker requires an exact entrypoint provider binding")?;
        }
    }
    require(expected == release.packages, "release package list differs from signed package records")?;
    require(targets.resource_dependencies.keys().all(|digest| closure.contains(digest)), "resource dependency owner not in closure")?;
    let mut bindings = BTreeSet::new();
    for binding in &release.provider_bindings {
        require(binding.scope_id == release.scope_id && bindings.insert((&binding.contract_id, binding.contract_version)), "binding scope mismatch or duplicate binding")?;
        let package = targets.packages.get(&binding.provider_digest).ok_or_else(|| crate::ReleaseError::Rejected("provider not in exact closure".into()))?;
        require(package.manifest.id == binding.provider_module_id && package.manifest.provides.iter().any(|p| p.contract_id == binding.contract_id && p.version == binding.contract_version), "binding provider identity/contract mismatch")?;
        require(targets.roots.contains(&binding.provider_digest) || targets.packages.values().any(|p| p.manifest.requires.iter().any(|r| r.contract_id == binding.contract_id && !matches!(r.binding_scope.as_deref(), Some("platform" | "host")) && version_matches(&r.version_range, binding.contract_version).unwrap_or(false))), "unused provider binding")?;
    }
    let mut singletons = BTreeSet::new();
    for singleton in &release.singleton_requirements {
        require(!singleton.optional && singleton.binding_scope.as_deref() == Some("platform") && singletons.insert(&singleton.contract_id), "invalid singleton requirement")?;
        require(targets.packages.values().any(|p| p.manifest.requires.contains(singleton)), "unrelated singleton requirement")?;
    }
    Ok(())
}
fn dependency_closure(targets: &TargetsMetadata, policy: &VerificationPolicy) -> Result<BTreeSet<String>> {
    require(!targets.roots.is_empty(), "scope requires at least one root package")?;
    require(targets.roots.iter().collect::<BTreeSet<_>>().len() == targets.roots.len(), "duplicate scope root")?;
    let mut closure = BTreeSet::new();
    let mut pending = targets.roots.clone();
    while let Some(digest) = pending.pop() {
        if !closure.insert(digest.clone()) { continue; }
        let package = targets.packages.get(&digest).ok_or_else(|| crate::ReleaseError::Rejected(format!("missing exact dependency {digest}")))?;
        policy.validate(&package.manifest)?;
        if let Some(resources) = targets.resource_dependencies.get(&digest) {
            require(resources.iter().collect::<BTreeSet<_>>().len() == resources.len(), "duplicate resource dependency")?;
            pending.extend(resources.iter().cloned());
        }
        for required in &package.manifest.requires {
            // Host availability is checked against immutable built-ins and policy approval
            // by policy.validate; host services are not signed module providers.
            if required.binding_scope.as_deref() == Some("host") { continue; }
            if required.binding_scope.as_deref() == Some("platform") {
                require(required.optional || targets.release.singleton_requirements.contains(required), "missing platform singleton requirement")?;
                continue;
            }
            require(required.binding_scope.as_deref().map_or(true, |s| s == "scope" || s == targets.release.scope_id), "unsupported cross-scope dependency")?;
            let mut matching = Vec::new();
            for binding in &targets.release.provider_bindings {
                if binding.contract_id == required.contract_id && version_matches(&required.version_range, binding.contract_version)? { matching.push(binding); }
            }
            if matching.is_empty() && required.optional { continue; }
            require(matching.len() == 1, format!("required contract {} needs exactly one provider binding", required.contract_id))?;
            pending.push(matching[0].provider_digest.clone());
        }
    }
    Ok(closure)
}

fn host_satisfies_minimum(actual: &str, minimum: &str) -> bool {
    if actual == minimum { return !actual.trim().is_empty(); }
    let parse = |value: &str| {
        value.parse::<u64>().ok().map(|major| semver::Version::new(major, 0, 0)).or_else(|| semver::Version::parse(value).ok())
    };
    match (parse(actual), parse(minimum)) {
        (Some(actual), Some(minimum)) => actual >= minimum,
        // Opaque host ABI labels remain exact, as required by ArtifactStore.
        _ => false,
    }
}
