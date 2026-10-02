use std::{collections::{BTreeMap, BTreeSet}, fs::{self, File, OpenOptions}, path::{Path, PathBuf}, sync::Arc};
use fs2::FileExt;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use crate::{atomic_json, canonical, compose_scope, read_json, require, sha256, sign, trust_pinned_root, validate_package_record, validate_targets, verify_package, MetadataHeader, MetadataRef, PackageRecord, Result, RootMetadata, ScopePlan, Signed, SnapshotMetadata, TargetsMetadata, TimestampMetadata, TrustedRoot, VerificationPolicy, VerifiedScopeRelease};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoryBundle {
    pub root_rotations: Vec<Signed<RootMetadata>>,
    pub targets: BTreeMap<String, Signed<TargetsMetadata>>,
    pub snapshot: Signed<SnapshotMetadata>,
    pub timestamp: Signed<TimestampMetadata>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HighWater { version: u64, digest: String }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrustState {
    pinned_digest: String,
    root: Signed<RootMetadata>,
    metadata: BTreeMap<String, HighWater>,
    last_verified_time: u64,
}
impl TrustState {
    fn new(root: &TrustedRoot) -> Result<Self> {
        Ok(Self { pinned_digest: sha256(&canonical(root.envelope())?), root: root.envelope().clone(), metadata: BTreeMap::new(), last_verified_time: 0 })
    }
    fn check(&mut self, name: &str, version: u64, bytes: &[u8]) -> Result<()> {
        let digest = sha256(bytes);
        if let Some(previous) = self.metadata.get(name) {
            require(version >= previous.version, format!("metadata rollback for {name}"))?;
            require(version != previous.version || digest == previous.digest, format!("metadata equivocation for {name}"))?;
        }
        self.metadata.insert(name.into(), HighWater { version, digest });
        Ok(())
    }
}

/// Trust high-water marks only; active environments and activation are owned by updates.
#[derive(Debug, Clone)]
pub struct RepositoryVerifier { state_path: PathBuf, pinned_root: TrustedRoot }
impl RepositoryVerifier {
    pub fn new(state_path: impl Into<PathBuf>, pinned_root: TrustedRoot) -> Self { Self { state_path: state_path.into(), pinned_root } }
    /// Allows a persisted newer root or a signed rotation chain to recover an
    /// expired bootstrap pin without trusting expired release metadata.
    pub fn from_pinned_file(state_path: impl Into<PathBuf>, pinned_file: &Path) -> Result<Self> {
        Ok(Self::new(state_path, crate::load_root_rotation_anchor(pinned_file)?))
    }
    pub fn verify(&self, bundle: &RepositoryBundle, policy: &VerificationPolicy, now: u64) -> Result<Vec<VerifiedScopeRelease>> {
        self.verify_selected(bundle, policy, now, None)
    }
    pub fn verify_scope(&self, bundle: &RepositoryBundle, scope_id: &str, channel: &str, policy: &VerificationPolicy, now: u64) -> Result<VerifiedScopeRelease> {
        let selected = BTreeSet::from([crate::scope_metadata_key(scope_id, channel, &policy.target)?]);
        let mut releases = self.verify_selected(bundle, policy, now, Some(&selected))?;
        releases.pop().ok_or_else(|| crate::ReleaseError::Rejected("scope/channel not found in signed snapshot".into()))
    }
    fn verify_selected(&self, bundle: &RepositoryBundle, policy: &VerificationPolicy, now: u64, selected: Option<&BTreeSet<String>>) -> Result<Vec<VerifiedScopeRelease>> {
        let parent = crate::parent_directory(&self.state_path);
        fs::create_dir_all(parent)?;
        let lock_path = self.state_path.with_extension("release-lock");
        let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(lock_path)?;
        lock.lock_exclusive()?;
        let mut state = load_state(&self.state_path, &self.pinned_root)?;
        let (_, releases) = verify_bundle(&mut state, bundle, policy, now, selected)?;
        atomic_json(&self.state_path, &state)?;
        Ok(releases)
    }
    pub fn trusted_root(&self, now: u64) -> Result<TrustedRoot> {
        let state = load_state(&self.state_path, &self.pinned_root)?;
        trust_pinned_root(state.root, now)
    }
    /// Keys only for an already-confirmed immutable closure, including offline.
    /// This does not verify a release or authorize downloading/activating a new scope.
    pub fn confirmed_artifact_trust_store(&self) -> Result<mintcat_runtime_artifacts::TrustStore> {
        let state = load_state(&self.state_path, &self.pinned_root)?;
        trust_pinned_root(state.root, 0)?.artifact_trust_store()
    }
}
fn load_state(path: &Path, pinned: &TrustedRoot) -> Result<TrustState> {
    if !path.exists() { return TrustState::new(pinned); }
    let state: TrustState = read_json(path)?;
    require(state.pinned_digest == sha256(&canonical(pinned.envelope())?), "configured trust pin differs from persisted trust; explicit trust migration required")?;
    require(state.root.signed.header.version >= pinned.version(), "persisted root rollback")?;
    // State lives in the runtime owner's private storage. It is not downloaded metadata.
    trust_pinned_root(state.root.clone(), 0)?;
    Ok(state)
}
fn verify_bundle(state: &mut TrustState, bundle: &RepositoryBundle, policy: &VerificationPolicy, now: u64, selected: Option<&BTreeSet<String>>) -> Result<(Arc<TrustedRoot>, Vec<VerifiedScopeRelease>)> {
    require(now >= state.last_verified_time, "clock moved behind persisted verification time")?;
    let mut root = trust_pinned_root(state.root.clone(), 0)?;
    for rotation in &bundle.root_rotations {
        if rotation.signed.header.version < root.version() { continue; }
        if rotation.signed.header.version == root.version() {
            require(rotation == root.envelope(), "root same-version equivocation")?;
            continue;
        }
        root = root.rotate(rotation.clone(), now)?;
    }
    require(root.expires() > now, "trusted root expired")?;
    let root = Arc::new(root);
    bundle.timestamp.signed.header.validate("timestamp", now)?;
    root.verify("timestamp", &bundle.timestamp)?;
    state.check("timestamp", bundle.timestamp.signed.header.version, &canonical(&bundle.timestamp)?)?;
    bundle.snapshot.signed.header.validate("snapshot", now)?;
    root.verify("snapshot", &bundle.snapshot)?;
    bundle.timestamp.signed.snapshot.verify(bundle.snapshot.signed.header.version, &bundle.snapshot)?;
    state.check("snapshot", bundle.snapshot.signed.header.version, &canonical(&bundle.snapshot)?)?;
    require(bundle.targets.keys().eq(bundle.snapshot.signed.targets.keys()), "snapshot target list mismatch")?;
    if let Some(selected) = selected { require(selected.iter().all(|name| bundle.targets.contains_key(name)), "selected scope/channel missing from signed snapshot")?; }
    for previous in state.metadata.keys().filter(|key| key.starts_with("targets:")) {
        require(bundle.targets.contains_key(&previous[8..]), "snapshot dropped a previously trusted scope/channel")?;
    }
    let mut releases = Vec::new();
    for (name, targets) in &bundle.targets {
        require(*name == targets.signed.release.signature_metadata_ref, "targets metadata key mismatch")?;
        bundle.snapshot.signed.targets[name].verify(targets.signed.header.version, targets)?;
        if selected.map_or(false, |names| !names.contains(name)) { continue; }
        targets.signed.header.validate("targets", now)?;
        root.verify("targets", targets)?;
        validate_targets(&targets.signed, policy)?;
        for package in targets.signed.packages.values() { validate_package_record(package, &root, policy)?; }
        state.check(&format!("targets:{name}"), targets.signed.header.version, &canonical(targets)?)?;
        let expires = root.expires().min(bundle.timestamp.signed.header.expires).min(bundle.snapshot.signed.header.expires).min(targets.signed.header.expires);
        releases.push(VerifiedScopeRelease::from_verified(targets.clone(), Arc::clone(&root), expires));
    }
    state.root = root.envelope().clone();
    state.last_verified_time = now;
    Ok((root, releases))
}

pub fn compose_bundle(previous: Option<&RepositoryBundle>, plan: ScopePlan, available: Vec<PackageRecord>, policy: &VerificationPolicy, root: &TrustedRoot, keys: &[SigningKey], version: u64, expires: u64, now: u64) -> Result<RepositoryBundle> {
    let mut targets = previous.map(|b| b.targets.clone()).unwrap_or_default();
    let metadata = compose_scope(plan, available, policy, expires)?;
    let name = metadata.release.signature_metadata_ref.clone();
    if let Some(old) = targets.get(&name) { require(metadata.release.sequence > old.signed.release.sequence, "scope sequence must increase")?; }
    let signed = sign(metadata, keys)?;
    root.verify("targets", &signed)?;
    targets.insert(name.clone(), signed);
    if let Some(previous) = previous { require(version > previous.snapshot.signed.header.version && version > previous.timestamp.signed.header.version, "snapshot/timestamp versions must increase")?; }
    let references = targets.iter().map(|(name, value)| Ok((name.clone(), MetadataRef::new(value.signed.header.version, value)?))).collect::<Result<BTreeMap<_, _>>>()?;
    let snapshot = sign(SnapshotMetadata { header: MetadataHeader { kind: "snapshot".into(), schema_version: 1, version, expires }, targets: references }, keys)?;
    let timestamp = sign(TimestampMetadata { header: MetadataHeader { kind: "timestamp".into(), schema_version: 1, version, expires }, snapshot: MetadataRef::new(version, &snapshot)? }, keys)?;
    let bundle = RepositoryBundle { root_rotations: previous.map(|b| b.root_rotations.clone()).unwrap_or_default(), targets, snapshot, timestamp };
    let mut state = TrustState::new(root)?;
    verify_bundle(&mut state, &bundle, policy, now, Some(&BTreeSet::from([name])))?;
    Ok(bundle)
}

#[derive(Debug, Clone)]
pub struct LocalRepository { root: PathBuf }
impl LocalRepository {
    /// Deliberately filesystem-only. This tool has no OSS credentials or remote pointer API.
    pub fn open(path: &Path) -> Result<Self> {
        require(!path.to_string_lossy().contains("://"), "repository must be a local filesystem path")?;
        fs::create_dir_all(path)?;
        let metadata = fs::symlink_metadata(path)?;
        require(metadata.is_dir() && !metadata.file_type().is_symlink(), "repository must be a real directory")?;
        let root = fs::canonicalize(path)?;
        let marker = root.join(".mintcat-local-release");
        if !marker.exists() {
            require(fs::read_dir(&root)?.next().is_none(), "refusing to initialize a nonempty unmarked repository")?;
            let mut file = OpenOptions::new().create_new(true).write(true).open(&marker)?;
            use std::io::Write;
            file.write_all(b"mintcat-local-release-v1\n")?;
            file.sync_all()?;
        }
        require(fs::read(&marker)? == b"mintcat-local-release-v1\n", "invalid local repository marker")?;
        Ok(Self { root })
    }
    pub fn path(&self) -> &Path { &self.root }
    fn lock(&self) -> Result<File> {
        let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(self.root.join(".publish-lock"))?;
        lock.lock_exclusive()?;
        Ok(lock)
    }
    pub fn current_bundle(&self) -> Result<Option<RepositoryBundle>> {
        // Publishers finish immutable artifacts/metadata before atomically
        // replacing current.json. Readers need one complete pointer snapshot,
        // not a writable publisher lock inside installed/read-only resources.
        let pointer = self.root.join("current.json");
        if pointer.exists() { Ok(Some(read_json(&pointer)?)) } else { Ok(None) }
    }
    pub fn publish_package(&self, source: &Path, root: &TrustedRoot, policy: &VerificationPolicy) -> Result<PackageRecord> {
        let _lock = self.lock()?;
        self.publish_package_locked(source, root, policy)
    }
    fn publish_package_locked(&self, source: &Path, root: &TrustedRoot, policy: &VerificationPolicy) -> Result<PackageRecord> {
        let package = verify_package(source, root, policy)?;
        let index_path = self.root.join("identities.json");
        let mut index: BTreeMap<String, PackageRecord> = if index_path.exists() { read_json(&index_path)? } else { BTreeMap::new() };
        let identity = sha256(&canonical(&(&package.manifest.id, &package.manifest.version, &package.manifest.target))?);
        if let Some(existing) = index.get(&identity) { require(*existing == package, "immutable package identity collision: differing bytes")?; }
        let destination = self.package_path(&package.digest)?;
        if destination.exists() {
            require(verify_package(&destination, root, policy)? == package, "immutable digest object collision")?;
        } else {
            let parent = destination.parent().expect("artifact parent");
            fs::create_dir_all(parent)?;
            let stage = parent.join(format!(".publish-{}", uuid::Uuid::new_v4()));
            let result: Result<()> = (|| {
                fs::create_dir(&stage)?;
                crate::copy_payload(&source.join("payload"), &stage.join("payload"), &package.manifest)?;
                fs::write(stage.join("module.json"), canonical(&package.manifest)?)?;
                fs::write(stage.join("signature.json"), canonical(&package.signature)?)?;
                require(verify_package(&stage, root, policy)? == package, "package changed during local publication")?;
                crate::sync_package(&stage)?;
                crate::commit_rename(&stage, &destination)?;
                Ok(())
            })();
            if stage.exists() { let _ = fs::remove_dir_all(&stage); }
            result?;
        }
        index.insert(identity, package.clone());
        atomic_json(&index_path, &index)?;
        Ok(package)
    }
    pub fn package_path(&self, digest: &str) -> Result<PathBuf> {
        require(digest.starts_with("sha256:") && digest.len() == 71 && digest[7..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "invalid artifact digest")?;
        Ok(self.root.join("artifacts").join("sha256").join(&digest[7..]))
    }
    pub fn publish_bundle(&self, bundle: &RepositoryBundle, package_sources: &[PathBuf], pinned: &TrustedRoot, policy: &VerificationPolicy, now: u64) -> Result<Vec<VerifiedScopeRelease>> {
        let _lock = self.lock()?;
        let state_path = self.root.join("trust-state.json");
        let mut state = load_state(&state_path, pinned)?;
        let current_path = self.root.join("current.json");
        let current: Option<RepositoryBundle> = if current_path.exists() { Some(read_json(&current_path)?) } else { None };
        let changed = bundle.targets.iter().filter(|(name, metadata)| current.as_ref().and_then(|previous| previous.targets.get(*name)) != Some(*metadata)).map(|(name, _)| name.clone()).collect::<BTreeSet<_>>();
        let (root, releases) = verify_bundle(&mut state, bundle, policy, now, Some(&changed))?;
        let mut expected = BTreeMap::new();
        for release in &releases {
            for (digest, package) in release.package_records() {
                if let Some(previous) = expected.insert(digest.as_str(), package) { require(previous == package, "scopes disagree about an immutable package")?; }
            }
        }
        // Reject unrelated sources before any write, then publish immutable bytes first.
        // Failure never advances the channel pointer.
        for source in package_sources {
            let package = verify_package(source, &root, policy)?;
            require(expected.get(package.digest.as_str()).map_or(false, |signed| **signed == package), "package source is not in the signed repository closures")?;
        }
        for source in package_sources { self.publish_package_locked(source, &root, policy)?; }
        for release in &releases {
            for package in release.package_records().values() {
                require(verify_package(&self.package_path(&package.digest)?, &root, policy)? == *package, "repository package differs from signed release")?;
            }
        }
        // Metadata objects are immutable digest paths; only local current.json is mutable.
        for name in &changed { self.put_metadata(&bundle.targets[name])?; }
        self.put_metadata(&bundle.snapshot)?;
        self.put_metadata(&bundle.timestamp)?;
        // Commit high-water first: crash may leave an older pointer, never allow rollback.
        atomic_json(&state_path, &state)?;
        atomic_json(&self.root.join("current.json"), bundle)?;
        Ok(releases)
    }
    fn put_metadata<T: Serialize>(&self, metadata: &Signed<T>) -> Result<()> {
        let bytes = canonical(metadata)?;
        let digest = sha256(&bytes);
        let path = self.root.join("metadata").join(format!("{}.json", &digest[7..]));
        fs::create_dir_all(path.parent().expect("metadata parent"))?;
        if path.exists() {
            let metadata = fs::symlink_metadata(&path)?;
            require(metadata.is_file() && !metadata.file_type().is_symlink() && fs::read(&path)? == bytes, "immutable metadata collision")?;
        } else {
            atomic_json(&path, metadata)?;
        }
        Ok(())
    }
}
