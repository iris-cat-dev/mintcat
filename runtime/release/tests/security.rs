use std::{collections::BTreeMap, fs, path::Path};
use ed25519_dalek::SigningKey;
use mintcat_runtime_release::*;
use mintcat_sdk::{ModuleActivation, ModuleComponent, ModuleFile, ModuleManifest, ModuleState};

fn keys() -> Vec<SigningKey> { vec![SigningKey::from_bytes(&[11; 32]), SigningKey::from_bytes(&[22; 32])] }
fn root_spec(version: u64, keys: &[SigningKey]) -> RootMetadata {
    let public = keys.iter().map(|k| (key_id(&k.verifying_key()), encode_public_key(&k.verifying_key()))).collect::<BTreeMap<_, _>>();
    RootMetadata {
        header: MetadataHeader { kind: "root".into(), schema_version: 1, version, expires: 10000 },
        roles: ["root", "targets", "snapshot", "timestamp"].into_iter().map(|role| (role.into(), Role { key_ids: public.keys().cloned().collect(), threshold: 2 })).collect(),
        keys: public,
        publishers: BTreeMap::from([("example.publisher".into(), BTreeMap::from([("package-key".into(), encode_public_key(&keys[0].verifying_key()))]))]),
    }
}
fn root() -> TrustedRoot { trust_pinned_root(sign(root_spec(1, &keys()), &keys()).unwrap(), 100).unwrap() }
fn policy() -> VerificationPolicy {
    VerificationPolicy { target: "x86_64-pc-windows-msvc".into(), host_api: "1".into(), max_data_schema: 1, permissions: vec![], contracts: BTreeMap::new() }
}
fn manifest(bytes: &[u8]) -> ModuleManifest {
    ModuleManifest {
        schema_version: 1, id: "example.resource".into(), version: "1.0.0".into(), publisher: "example.publisher".into(),
        target: policy().target, host_api: "1".into(), components: vec![ModuleComponent { kind: "resource".into(), entry: "data.bin".into(), runtime: "data".into(), state_model: "stateless".into() }],
        provides: vec![], requires: vec![], permissions: vec![], contributes: serde_json::json!({}),
        state: ModuleState { schema_version: 1, readable_range: "1".into(), writable_range: "1".into(), rollback_before_write_only: Some(true) },
        activation: ModuleActivation { trigger: "on-demand".into(), drain_policy: "finish-requests".into() },
        files: vec![ModuleFile { path: "data.bin".into(), size: bytes.len() as u64, sha256: sha256(bytes), executable: false }],
    }
}
fn package(base: &Path, name: &str, bytes: &[u8]) -> (std::path::PathBuf, PackageRecord) {
    let source = base.join(format!("{name}-source"));
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data.bin"), bytes).unwrap();
    let output = base.join(name);
    let record = pack(&source, manifest(bytes), "package-key", &keys()[0], &policy(), &output).unwrap();
    (output, record)
}
fn plan(package: &PackageRecord, scope: &str, sequence: u64) -> ScopePlan {
    ScopePlan { scope_id: scope.into(), release_id: format!("{scope}-r{sequence}"), sequence, channel: "stable".into(), target: policy().target, min_host_api: "1".into(), roots: vec![package.digest.clone()], provider_bindings: vec![], singleton_requirements: vec![], resource_dependencies: BTreeMap::new(), cohort: CohortPolicy { salt: "stable-installation-cohort".into(), basis_points: 1500 } }
}
fn bundle(record: &PackageRecord, scope: &str, sequence: u64, version: u64, previous: Option<&RepositoryBundle>) -> RepositoryBundle {
    legacy_bundle(previous, plan(record, scope, sequence), vec![record.clone()], &policy(), &root(), &keys(), version, 9000, 100).unwrap()
}

// Historical schema1 fixtures are constructed directly. Production composition
// deliberately cannot issue these old resource/platform/source closures anymore.
fn legacy_targets(plan: ScopePlan, available: Vec<PackageRecord>, _policy: &VerificationPolicy, expires: u64) -> Result<TargetsMetadata> {
    let signature_metadata_ref = scope_metadata_key(&plan.scope_id, &plan.channel, &plan.target)?;
    Ok(TargetsMetadata { header: MetadataHeader { kind: "targets".into(), schema_version: 1, version: plan.sequence, expires },
        release: mintcat_sdk::ScopeReleaseSet { schema_version: 1, scope_id: plan.scope_id, release_id: plan.release_id, sequence: plan.sequence, channel: plan.channel, target: plan.target, min_host_api: plan.min_host_api,
            packages: available.iter().map(|record| mintcat_sdk::ReleasePackage { module_id: record.manifest.id.clone(), version: record.manifest.version.clone(), target: record.manifest.target.clone(), digest: record.digest.clone(), schema_version: record.manifest.schema_version }).collect(),
            provider_bindings: plan.provider_bindings, singleton_requirements: plan.singleton_requirements, signature_metadata_ref },
        cohort: plan.cohort, roots: plan.roots, resource_dependencies: plan.resource_dependencies,
        packages: available.into_iter().map(|record| (record.digest.clone(), record)).collect() })
}

fn legacy_bundle(previous: Option<&RepositoryBundle>, plan: ScopePlan, available: Vec<PackageRecord>, policy: &VerificationPolicy, root: &TrustedRoot, keys: &[SigningKey], version: u64, expires: u64, now: u64) -> Result<RepositoryBundle> {
    let scope = plan.scope_id.clone(); let channel = plan.channel.clone();
    let metadata = legacy_targets(plan, available, policy, expires)?;
    let mut targets = previous.map(|bundle| bundle.targets.clone()).unwrap_or_default();
    targets.insert(metadata.release.signature_metadata_ref.clone(), sign(metadata, keys)?);
    let references = targets.iter().map(|(name, value)| Ok((name.clone(), MetadataRef::new(value.signed.header.version, value)?))).collect::<Result<BTreeMap<_, _>>>()?;
    let snapshot = sign(SnapshotMetadata { header: MetadataHeader { kind: "snapshot".into(), schema_version: 1, version, expires }, targets: references }, keys)?;
    let timestamp = sign(TimestampMetadata { header: MetadataHeader { kind: "timestamp".into(), schema_version: 1, version, expires }, snapshot: MetadataRef::new(version, &snapshot)? }, keys)?;
    let bundle = RepositoryBundle { root_rotations: previous.map(|bundle| bundle.root_rotations.clone()).unwrap_or_default(), targets, snapshot, timestamp };
    let sandbox = tempfile::tempdir()?;
    RepositoryVerifier::new(sandbox.path().join("state.json"), root.clone()).verify_scope(&bundle, &scope, &channel, policy, now)?;
    Ok(bundle)
}

#[test]
fn installed_repository_snapshot_reads_do_not_create_publisher_state() {
    let sandbox = tempfile::tempdir().unwrap();
    let (path, record) = package(sandbox.path(), "offline", b"trusted offline resource");
    let signed = bundle(&record, "game:drg", 1, 1, None);
    let repository = LocalRepository::open(&sandbox.path().join("repository")).unwrap();
    repository.publish_bundle(&signed, &[path], &root(), &policy(), 100).unwrap();
    let publisher_lock = repository.path().join(".publish-lock");
    fs::remove_file(&publisher_lock).unwrap();

    let installed = LocalRepository::open(repository.path()).unwrap();
    assert_eq!(installed.current_bundle().unwrap(), Some(signed));
    assert!(!publisher_lock.exists(), "reading an installed baseline must not require or create writable publisher state");
    assert_eq!(fs::read(installed.package_path(&record.digest).unwrap().join("payload/data.bin")).unwrap(), b"trusted offline resource");
}

#[test]
fn rotation_requires_old_and_new_thresholds_and_exact_next_version() {
    let old = keys();
    let new = vec![SigningKey::from_bytes(&[33; 32]), SigningKey::from_bytes(&[44; 32])];
    let root = root();
    let spec = root_spec(2, &new);
    assert!(root.rotate(sign(spec.clone(), &old).unwrap(), 100).is_err());
    assert!(root.rotate(sign(spec.clone(), &new).unwrap(), 100).is_err());
    let mut combined = old;
    combined.extend(new);
    let next = root.rotate(sign(spec.clone(), &combined).unwrap(), 100).unwrap();
    assert_eq!(next.version(), 2);
    assert!(root.rotate(sign(root_spec(3, &combined), &combined).unwrap(), 100).is_err());
    let mut insufficient = sign(spec, &combined).unwrap();
    insufficient.signatures.pop();
    assert!(root.rotate(insufficient, 100).is_err());
}

#[test]
fn rollback_and_equivocation_survive_verifier_restart() {
    let sandbox = tempfile::tempdir().unwrap();
    let (_, record) = package(sandbox.path(), "pack", b"trusted payload");
    let first = bundle(&record, "game:drg", 1, 1, None);
    let second = bundle(&record, "game:drg", 2, 2, Some(&first));
    let state = sandbox.path().join("client-trust.json");
    RepositoryVerifier::new(&state, root()).verify(&second, &policy(), 100).unwrap();
    let restarted = RepositoryVerifier::new(&state, root());
    assert!(restarted.verify(&first, &policy(), 101).is_err());
    assert!(restarted.verify(&second, &policy(), 99).is_err());
    let mut equivocation = second.clone();
    let name = equivocation.targets.keys().next().unwrap().clone();
    let mut targets = equivocation.targets[&name].signed.clone();
    targets.cohort.basis_points = 9000;
    equivocation.targets.insert(name.clone(), sign(targets, &keys()).unwrap());
    equivocation.snapshot.signed.header.version = 3;
    equivocation.snapshot.signed.targets.insert(name.clone(), MetadataRef::new(2, &equivocation.targets[&name]).unwrap());
    equivocation.snapshot = sign(equivocation.snapshot.signed, &keys()).unwrap();
    equivocation.timestamp.signed.header.version = 3;
    equivocation.timestamp.signed.snapshot = MetadataRef::new(3, &equivocation.snapshot).unwrap();
    equivocation.timestamp = sign(equivocation.timestamp.signed, &keys()).unwrap();
    assert!(restarted.verify(&equivocation, &policy(), 101).is_err());
    assert!(restarted.verify(&second, &policy(), 9000).is_err());
    restarted.verify(&second, &policy(), 101).unwrap();
}

#[test]
fn signed_wrong_metadata_lengths_and_package_lengths_are_rejected() {
    let sandbox = tempfile::tempdir().unwrap();
    let (_, record) = package(sandbox.path(), "pack", b"trusted payload");
    let first = bundle(&record, "game:drg", 1, 1, None);
    let mut bad = first.clone();
    bad.timestamp.signed.snapshot.length += 1;
    bad.timestamp = sign(bad.timestamp.signed, &keys()).unwrap();
    let verifier = RepositoryVerifier::new(sandbox.path().join("state.json"), root());
    assert!(verifier.verify(&bad, &policy(), 100).is_err());
    let mut bad_record = record;
    bad_record.length += 1;
    assert!(legacy_bundle(None, plan(&bad_record, "game:drg", 1), vec![bad_record], &policy(), &root(), &keys(), 1, 9000, 100).is_err());
    verifier.verify(&first, &policy(), 100).unwrap();
}

#[test]
fn package_collisions_and_tampering_do_not_replace_published_bytes() {
    let sandbox = tempfile::tempdir().unwrap();
    let (first, record) = package(sandbox.path(), "first", b"original");
    let (second, _) = package(sandbox.path(), "second", b"modified");
    let repository = LocalRepository::open(&sandbox.path().join("repository")).unwrap();
    repository.publish_package(&first, &root(), &policy()).unwrap();
    assert!(repository.publish_package(&second, &root(), &policy()).is_err());
    assert_eq!(fs::read(repository.package_path(&record.digest).unwrap().join("payload/data.bin")).unwrap(), b"original");
    repository.publish_package(&first, &root(), &policy()).unwrap();
    fs::write(first.join("payload/data.bin"), b"tampered").unwrap();
    assert!(verify_package(&first, &root(), &policy()).is_err());
    fs::write(first.join("payload/data.bin"), b"short").unwrap();
    assert!(verify_package(&first, &root(), &policy()).is_err());
}

#[test]
fn scope_update_preserves_unrelated_game_metadata_and_stable_cohorts() {
    let sandbox = tempfile::tempdir().unwrap();
    let (_, record) = package(sandbox.path(), "pack", b"trusted payload");
    let drg = bundle(&record, "game:drg", 1, 1, None);
    let both = bundle(&record, "game:rogue-core", 1, 2, Some(&drg));
    let update = bundle(&record, "game:drg", 2, 3, Some(&both));
    let rc = scope_metadata_key("game:rogue-core", "stable", &policy().target).unwrap();
    assert_eq!(update.targets[&rc], both.targets[&rc]);
    let verifier = RepositoryVerifier::new(sandbox.path().join("state.json"), root());
    verifier.verify(&both, &policy(), 100).unwrap();
    let releases = verifier.verify(&update, &policy(), 101).unwrap();
    let rc_release = releases.iter().find(|s| s.release().scope_id == "game:rogue-core").unwrap();
    assert_eq!(rc_release.release().sequence, 1);
    assert!(CohortPolicy { salt: "salt".into(), basis_points: 10000 }.includes("game:drg", "stable", "install-123").unwrap());
    assert!(!CohortPolicy { salt: "salt".into(), basis_points: 0 }.includes("game:drg", "stable", "install-123").unwrap());
}

#[test]
fn exact_dependency_closure_rejects_missing_or_foreign_scope_resources() {
    let sandbox = tempfile::tempdir().unwrap();
    let (_, record) = package(sandbox.path(), "pack", b"trusted payload");
    let mut plan = plan(&record, "game:drg", 1);
    plan.resource_dependencies.insert(record.digest.clone(), vec![format!("sha256:{}", "0".repeat(64))]);
    assert!(legacy_bundle(None, plan, vec![record.clone()], &policy(), &root(), &keys(), 1, 9000, 100).is_err());
    let mut foreign = record.clone();
    foreign.manifest.contributes = serde_json::json!({"scopeId": "game:rogue-core"});
    assert!(legacy_bundle(None, plan_for_foreign(&foreign), vec![foreign], &policy(), &root(), &keys(), 1, 9000, 100).is_err());
}
fn plan_for_foreign(record: &PackageRecord) -> ScopePlan { plan(record, "game:drg", 1) }

#[test]
fn expired_unrelated_scope_does_not_block_selected_game_update() {
    let sandbox = tempfile::tempdir().unwrap();
    let (path, record) = package(sandbox.path(), "pack", b"trusted payload");
    let rc = legacy_bundle(None, plan(&record, "game:rogue-core", 1), vec![record.clone()], &policy(), &root(), &keys(), 1, 200, 100).unwrap();
    let repository = LocalRepository::open(&sandbox.path().join("repository")).unwrap();
    repository.publish_bundle(&rc, &[path.clone()], &root(), &policy(), 100).unwrap();
    let drg = legacy_bundle(Some(&rc), plan(&record, "game:drg", 1), vec![record], &policy(), &root(), &keys(), 2, 9000, 300).unwrap();
    let rc_name = scope_metadata_key("game:rogue-core", "stable", &policy().target).unwrap();
    assert_eq!(drg.targets[&rc_name], rc.targets[&rc_name]);
    let published = repository.publish_bundle(&drg, &[path], &root(), &policy(), 300).unwrap();
    assert_eq!(published[0].release().scope_id, "game:drg");
    let verifier = RepositoryVerifier::new(sandbox.path().join("state.json"), root());
    assert!(verifier.verify(&drg, &policy(), 300).is_err());
    let selected = verifier.verify_scope(&drg, "game:drg", "stable", &policy(), 300).unwrap();
    assert_eq!(selected.release().sequence, 1);
    assert!(verifier.verify_scope(&drg, "game:rogue-core", "stable", &policy(), 300).is_err());
}

#[test]
fn development_origin_is_rejected_even_with_an_authorized_package_key() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data.bin"), b"development payload").unwrap();
    let mut manifest = manifest(b"development payload");
    manifest.contributes = serde_json::json!({"origin": "development"});
    let output = sandbox.path().join("development");
    let record = pack(&source, manifest, "package-key", &keys()[0], &policy(), &output).unwrap();
    assert!(verify_package(&output, &root(), &policy()).is_err());
    assert!(legacy_bundle(None, plan(&record, "game:drg", 1), vec![record], &policy(), &root(), &keys(), 1, 9000, 100).is_err());
}

#[test]
fn native_image_target_must_match_actual_format_and_architecture() {
    let binary = std::env::current_exe().unwrap();
    let host = target_lexicon::HOST;
    validate_native_payload(&binary, &host.to_string()).unwrap();
    let mut other_arch = host.clone();
    other_arch.architecture = if host.architecture.to_string() == "aarch64" { "x86_64".parse().unwrap() } else { "aarch64".parse().unwrap() };
    assert!(validate_native_payload(&binary, &other_arch.to_string()).is_err());
    let other_os = if cfg!(windows) { "aarch64-apple-darwin" } else { "x86_64-pc-windows-msvc" };
    assert!(validate_native_payload(&binary, other_os).is_err());
    assert!(validate_native_payload(&binary, &host.architecture.to_string()).is_err());
}

#[test]
fn role_keys_are_not_interchangeable_and_rotated_root_persists() {
    let sandbox = tempfile::tempdir().unwrap();
    let (_, record) = package(sandbox.path(), "pack", b"trusted payload");
    let old = keys();
    let online = vec![SigningKey::from_bytes(&[55; 32]), SigningKey::from_bytes(&[66; 32])];
    let mut spec = root_spec(1, &old);
    for key in &online { spec.keys.insert(key_id(&key.verifying_key()), encode_public_key(&key.verifying_key())); }
    let online_ids = online.iter().map(|key| key_id(&key.verifying_key())).collect::<std::collections::BTreeSet<_>>();
    for role in ["targets", "snapshot", "timestamp"] { spec.roles.get_mut(role).unwrap().key_ids = online_ids.clone(); }
    let pinned = trust_pinned_root(sign(spec, &old).unwrap(), 100).unwrap();
    let metadata = legacy_targets(plan(&record, "game:drg", 1), vec![record.clone()], &policy(), 9000).unwrap();
    assert!(pinned.verify("targets", &sign(metadata.clone(), &old).unwrap()).is_err());
    pinned.verify("targets", &sign(metadata, &online).unwrap()).unwrap();
    let first = legacy_bundle(None, plan(&record, "game:drg", 1), vec![record.clone()], &policy(), &pinned, &online, 1, 9000, 100).unwrap();
    let next_keys = vec![SigningKey::from_bytes(&[77; 32]), SigningKey::from_bytes(&[88; 32])];
    let mut next_spec = root_spec(2, &next_keys);
    next_spec.publishers = pinned.envelope().signed.publishers.clone();
    let mut rotation_signers = old;
    rotation_signers.extend(next_keys.iter().cloned());
    let rotation = sign(next_spec, &rotation_signers).unwrap();
    let rotated = pinned.rotate(rotation.clone(), 100).unwrap();
    let mut second = legacy_bundle(Some(&first), plan(&record, "game:drg", 2), vec![record], &policy(), &rotated, &next_keys, 2, 9000, 100).unwrap();
    second.root_rotations.push(rotation);
    let state = sandbox.path().join("trust.json");
    let verifier = RepositoryVerifier::new(&state, pinned.clone());
    let frozen = verifier.verify(&first, &policy(), 100).unwrap().pop().unwrap();
    verifier.verify(&second, &policy(), 100).unwrap();
    let restarted = RepositoryVerifier::new(&state, pinned);
    assert_eq!(restarted.trusted_root(101).unwrap().version(), 2);
    assert!(restarted.verify(&first, &policy(), 101).is_err());
    frozen.verified_root().verify("targets", frozen.targets()).unwrap();
    assert!(restarted.trusted_root(101).unwrap().verify("targets", frozen.targets()).is_err());
}

#[test]
fn expired_bootstrap_pin_recovers_only_through_an_authenticated_current_root() {
    let sandbox = tempfile::tempdir().unwrap();
    let (_, record) = package(sandbox.path(), "pack", b"trusted payload");
    let mut spec = root_spec(1, &keys());
    spec.header.expires = 150;
    let original = sign(spec, &keys()).unwrap();
    let pinned = trust_pinned_root(original.clone(), 100).unwrap();
    let first = legacy_bundle(None, plan(&record, "game:drg", 1), vec![record.clone()], &policy(), &pinned, &keys(), 1, 140, 100).unwrap();
    let next_keys = vec![SigningKey::from_bytes(&[99; 32]), SigningKey::from_bytes(&[111; 32])];
    let mut next_spec = root_spec(2, &next_keys);
    next_spec.publishers = pinned.envelope().signed.publishers.clone();
    let mut rotation_keys = keys();
    rotation_keys.extend(next_keys.iter().cloned());
    let rotation = sign(next_spec, &rotation_keys).unwrap();
    let rotated = pinned.rotate(rotation.clone(), 200).unwrap();
    let mut second = legacy_bundle(Some(&first), plan(&record, "game:drg", 2), vec![record], &policy(), &rotated, &next_keys, 2, 9000, 200).unwrap();
    second.root_rotations.push(rotation);
    let pin_path = sandbox.path().join("root.json");
    fs::write(&pin_path, serde_json::to_vec(&original).unwrap()).unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&pin_path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert!(load_trust_root(&pin_path, 200).is_err());
    let state = sandbox.path().join("trust.json");
    RepositoryVerifier::from_pinned_file(&state, &pin_path).unwrap().verify(&second, &policy(), 200).unwrap();
    let restarted = RepositoryVerifier::from_pinned_file(&state, &pin_path).unwrap();
    assert_eq!(restarted.trusted_root(201).unwrap().version(), 2);
    assert!(restarted.verify(&first, &policy(), 201).is_err());
    assert!(restarted.trusted_root(10001).is_err());
    assert!(restarted.verify(&second, &policy(), 10001).is_err());
}

#[test]
fn prepared_scope_proof_expires_with_timestamp_before_long_lived_targets() {
    let sandbox = tempfile::tempdir().unwrap();
    let (_, record) = package(sandbox.path(), "pack", b"trusted payload");
    let mut metadata = bundle(&record, "game:drg", 1, 1, None);
    metadata.timestamp.signed.header.expires = 150;
    metadata.timestamp = sign(metadata.timestamp.signed, &keys()).unwrap();
    let proof = RepositoryVerifier::new(sandbox.path().join("trust.json"), root()).verify_scope(&metadata, "game:drg", "stable", &policy(), 100).unwrap();
    assert!(proof.require_fresh(149).is_ok());
    assert!(proof.require_fresh(150).is_err());
}

#[test]
fn scope_minimum_host_does_not_relax_exact_package_host_abi() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data.bin"), b"host two payload").unwrap();
    let mut descriptor = manifest(b"host two payload");
    descriptor.host_api = "2".into();
    let mut host = policy();
    host.host_api = "2".into();
    let record = pack(&source, descriptor, "package-key", &keys()[0], &host, &sandbox.path().join("package")).unwrap();
    let metadata = legacy_bundle(None, plan(&record, "game:drg", 1), vec![record.clone()], &host, &root(), &keys(), 1, 9000, 100).unwrap();
    let state = sandbox.path().join("state.json");
    let verifier = RepositoryVerifier::new(&state, root());
    verifier.verify_scope(&metadata, "game:drg", "stable", &host, 100).unwrap();
    assert!(verifier.verify_scope(&metadata, "game:drg", "stable", &policy(), 100).is_err());
    let mut too_new = plan(&record, "game:drg", 2);
    too_new.min_host_api = "3".into();
    assert!(compose_scope(too_new, vec![record], &host, 9000).is_err());
}

#[test]
fn self_contained_game_release_issues_exact_data_and_rejects_obsolete_runtime_packages() {
    use mintcat_game_api::{GAME_ADAPTER_CONTRACT_ID, GAME_ADAPTER_CONTRACT_VERSION};
    use mintcat_sdk::{ProvidedContract, ScopedProviderBinding};
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("game-source");
    fs::create_dir(&source).unwrap();
    let data = b"TEST ONLY signed resource data";
    fs::write(source.join("data.bin"), data).unwrap();
    fs::copy(std::env::current_exe().unwrap(), source.join("worker")).unwrap();
    let worker = fs::read(source.join("worker")).unwrap();
    let mut policy = policy();
    policy.target = target_lexicon::HOST.to_string();
    policy.contracts.insert(GAME_ADAPTER_CONTRACT_ID.into(), [GAME_ADAPTER_CONTRACT_VERSION].into());
    let mut game = manifest(data);
    game.id = "example.game".into();
    game.target = policy.target.clone();
    game.components = vec![ModuleComponent { kind: "service".into(), runtime: "worker".into(), state_model: "stateless".into(), entry: "worker".into() }];
    game.provides = vec![ProvidedContract { contract_id: GAME_ADAPTER_CONTRACT_ID.into(), version: GAME_ADAPTER_CONTRACT_VERSION, cardinality: Some("multi".into()) }];
    game.files.push(ModuleFile { path: "worker".into(), size: worker.len() as u64, sha256: sha256(&worker), executable: true });
    // This native executable is a test harness, never an installable game artifact.
    game.contributes = serde_json::json!({"testOnly": true, "scopeId":"game:fixture", "games":[{
        "schemaVersion":1,"gameId":"game:fixture","displayName":"Fixture","adapterVersion":"1.0.0",
        "settingsSchemaVersion":1,"settingsSchema":{"type":"object"},
        "variants":[{"variantId":"fixture","displayName":"Fixture","store":"fixture","storeAppId":null,
            "target":"x86_64-pc-windows-msvc","mainPakRelativePath":"Content/base.pak","paksRelativePath":"Content",
            "binariesRelativePath":"Binaries","modPakRelativePath":"Content/mods.pak","processNames":["Fixture.exe"]}],
        "inputFormats":["pak"],"sourceBindings":[],"capabilities":["create-build-plan"],
        "resources":[{"resourceId":"fixture.data","versionRange":"1","optional":false,"purpose":"test data"}]
    }],"bundledResources":[{"resourceId":"fixture.data","version":"1.0.0","archivePath":"data.bin"}]});
    validate_game_manifest(&game).unwrap();
    let record = pack(&source, game.clone(), "package-key", &keys()[0], &policy, &sandbox.path().join("game-package")).unwrap();
    let mut plan = plan(&record, "game:fixture", 1);
    plan.target = policy.target.clone();
    plan.provider_bindings = vec![ScopedProviderBinding { scope_id: "game:fixture".into(), contract_id: GAME_ADAPTER_CONTRACT_ID.into(), contract_version: GAME_ADAPTER_CONTRACT_VERSION, provider_module_id: record.manifest.id.clone(), provider_digest: record.digest.clone() }];
    let signed = compose_bundle(None, plan.clone(), vec![record.clone()], &policy, &root(), &keys(), 1, 9000, 100).unwrap();
    let release = RepositoryVerifier::new(sandbox.path().join("state.json"), root()).verify_scope(&signed, "game:fixture", "stable", &policy, 100).unwrap();
    assert_eq!(release.release().schema_version, 2);
    assert_eq!(release.package_records().len(), 1);
    assert_eq!(release.package_records()[&record.digest].manifest, game);
    let mut missing = game.clone();
    missing.files.retain(|file| file.path != "data.bin");
    assert!(validate_game_manifest(&missing).unwrap_err().to_string().contains("archive data.bin missing"));
    let mut missing = game.clone();
    missing.contributes["bundledResources"] = serde_json::json!([]);
    assert!(validate_game_manifest(&missing).unwrap_err().to_string().contains("required game resource"));
    let mut obsolete = game.clone();
    obsolete.components.push(ModuleComponent { kind: "ui".into(), runtime: "webview".into(), state_model: "stateless".into(), entry: "data.bin".into() });
    assert!(validate_game_manifest(&obsolete).is_err());
    obsolete = game.clone();
    obsolete.requires.push(mintcat_sdk::RequiredContract { contract_id: "unreal.engine".into(), version_range: "1".into(), optional: false, binding_scope: Some("scope".into()) });
    assert!(validate_game_manifest(&obsolete).is_err());
    let mut obsolete_plan = plan;
    obsolete_plan.resource_dependencies.insert(record.digest.clone(), vec![record.digest.clone()]);
    assert!(compose_scope(obsolete_plan, vec![record], &policy, 9000).is_err());
}
