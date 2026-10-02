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
fn contract_package(base: &Path, name: &str, requirement: mintcat_sdk::RequiredContract, host: &VerificationPolicy) -> Result<PackageRecord> {
    let source = base.join(format!("{name}-source"));
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data.bin"), name.as_bytes()).unwrap();
    let mut descriptor = manifest(name.as_bytes());
    descriptor.id = name.into();
    descriptor.requires.push(requirement);
    pack(&source, descriptor, "package-key", &keys()[0], host, &base.join(name))
}
fn bundle(record: &PackageRecord, scope: &str, sequence: u64, version: u64, previous: Option<&RepositoryBundle>) -> RepositoryBundle {
    compose_bundle(previous, plan(record, scope, sequence), vec![record.clone()], &policy(), &root(), &keys(), version, 9000, 100).unwrap()
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
    assert!(compose_bundle(None, plan(&bad_record, "game:drg", 1), vec![bad_record], &policy(), &root(), &keys(), 1, 9000, 100).is_err());
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
    assert!(compose_scope(plan, vec![record.clone()], &policy(), 9000).is_err());
    let mut foreign = record.clone();
    foreign.manifest.contributes = serde_json::json!({"scopeId": "game:rogue-core"});
    assert!(compose_scope(plan_for_foreign(&foreign), vec![foreign], &policy(), 9000).is_err());
}
fn plan_for_foreign(record: &PackageRecord) -> ScopePlan { plan(record, "game:drg", 1) }

#[test]
fn expired_unrelated_scope_does_not_block_selected_game_update() {
    let sandbox = tempfile::tempdir().unwrap();
    let (path, record) = package(sandbox.path(), "pack", b"trusted payload");
    let rc = compose_bundle(None, plan(&record, "game:rogue-core", 1), vec![record.clone()], &policy(), &root(), &keys(), 1, 200, 100).unwrap();
    let repository = LocalRepository::open(&sandbox.path().join("repository")).unwrap();
    repository.publish_bundle(&rc, &[path.clone()], &root(), &policy(), 100).unwrap();
    let drg = compose_bundle(Some(&rc), plan(&record, "game:drg", 1), vec![record], &policy(), &root(), &keys(), 2, 9000, 300).unwrap();
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
    assert!(compose_bundle(None, plan(&record, "game:drg", 1), vec![record], &policy(), &root(), &keys(), 1, 9000, 100).is_err());
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
    let metadata = compose_scope(plan(&record, "game:drg", 1), vec![record.clone()], &policy(), 9000).unwrap();
    assert!(pinned.verify("targets", &sign(metadata.clone(), &old).unwrap()).is_err());
    pinned.verify("targets", &sign(metadata, &online).unwrap()).unwrap();
    let first = compose_bundle(None, plan(&record, "game:drg", 1), vec![record.clone()], &policy(), &pinned, &online, 1, 9000, 100).unwrap();
    let next_keys = vec![SigningKey::from_bytes(&[77; 32]), SigningKey::from_bytes(&[88; 32])];
    let mut next_spec = root_spec(2, &next_keys);
    next_spec.publishers = pinned.envelope().signed.publishers.clone();
    let mut rotation_signers = old;
    rotation_signers.extend(next_keys.iter().cloned());
    let rotation = sign(next_spec, &rotation_signers).unwrap();
    let rotated = pinned.rotate(rotation.clone(), 100).unwrap();
    let mut second = compose_bundle(Some(&first), plan(&record, "game:drg", 2), vec![record], &policy(), &rotated, &next_keys, 2, 9000, 100).unwrap();
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
    let first = compose_bundle(None, plan(&record, "game:drg", 1), vec![record.clone()], &policy(), &pinned, &keys(), 1, 140, 100).unwrap();
    let next_keys = vec![SigningKey::from_bytes(&[99; 32]), SigningKey::from_bytes(&[111; 32])];
    let mut next_spec = root_spec(2, &next_keys);
    next_spec.publishers = pinned.envelope().signed.publishers.clone();
    let mut rotation_keys = keys();
    rotation_keys.extend(next_keys.iter().cloned());
    let rotation = sign(next_spec, &rotation_keys).unwrap();
    let rotated = pinned.rotate(rotation.clone(), 200).unwrap();
    let mut second = compose_bundle(Some(&first), plan(&record, "game:drg", 2), vec![record], &policy(), &rotated, &next_keys, 2, 9000, 200).unwrap();
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
    let metadata = compose_bundle(None, plan(&record, "game:drg", 1), vec![record.clone()], &host, &root(), &keys(), 1, 9000, 100).unwrap();
    let state = sandbox.path().join("state.json");
    let verifier = RepositoryVerifier::new(&state, root());
    verifier.verify_scope(&metadata, "game:drg", "stable", &host, 100).unwrap();
    assert!(verifier.verify_scope(&metadata, "game:drg", "stable", &policy(), 100).is_err());
    let mut too_new = plan(&record, "game:drg", 2);
    too_new.min_host_api = "3".into();
    assert!(compose_scope(too_new, vec![record], &host, 9000).is_err());
}

#[test]
fn exact_contract_binding_selects_only_its_provider_and_rejects_identity_spoofing() {
    let sandbox = tempfile::tempdir().unwrap();
    let mut policy = policy();
    policy.contracts.insert("example.assets".into(), std::collections::BTreeSet::from([1]));
    let make = |id: &str, provides, requires| {
        let source = sandbox.path().join(format!("{id}-source"));
        fs::create_dir(&source).unwrap();
        fs::write(source.join("data.bin"), id.as_bytes()).unwrap();
        let mut manifest = manifest(id.as_bytes());
        manifest.id = id.into();
        manifest.provides = provides;
        manifest.requires = requires;
        pack(&source, manifest, "package-key", &keys()[0], &policy, &sandbox.path().join(id)).unwrap()
    };
    let provided = mintcat_sdk::ProvidedContract { contract_id: "example.assets".into(), version: 1, cardinality: Some("multi".into()) };
    let first = make("assets.first", vec![provided.clone()], vec![]);
    let unrelated = make("assets.second", vec![provided], vec![]);
    let consumer = make("assets.consumer", vec![], vec![mintcat_sdk::RequiredContract { contract_id: "example.assets".into(), version_range: "1".into(), optional: false, binding_scope: Some("scope".into()) }]);
    let mut exact = plan(&consumer, "game:drg", 1);
    exact.provider_bindings.push(mintcat_sdk::ScopedProviderBinding { scope_id: "game:drg".into(), contract_id: "example.assets".into(), contract_version: 1, provider_module_id: first.manifest.id.clone(), provider_digest: first.digest.clone() });
    let available = vec![consumer, first, unrelated.clone()];
    let targets = compose_scope(exact.clone(), available.clone(), &policy, 9000).unwrap();
    let ids = targets.release.packages.iter().map(|p| p.module_id.as_str()).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids, std::collections::BTreeSet::from(["assets.consumer", "assets.first"]));
    let mut missing = exact.clone();
    missing.provider_bindings.clear();
    assert!(compose_scope(missing, available.clone(), &policy, 9000).is_err());
    exact.provider_bindings[0].provider_digest = unrelated.digest;
    assert!(compose_scope(exact, available, &policy, 9000).is_err());
}

#[test]
fn signed_host_dependency_uses_approved_builtin_without_a_module_provider() {
    let sandbox = tempfile::tempdir().unwrap();
    let mut host = policy();
    host.contracts.insert("source.network".into(), [1].into());
    let requirement = mintcat_sdk::RequiredContract { contract_id: "source.network".into(), version_range: "=1".into(), optional: false, binding_scope: Some("host".into()) };
    let record = contract_package(sandbox.path(), "source.consumer", requirement.clone(), &host).unwrap();
    verify_package(&sandbox.path().join("source.consumer"), &root(), &host).unwrap();
    let metadata = compose_bundle(None, plan(&record, "game:drg", 1), vec![record.clone()], &host, &root(), &keys(), 1, 9000, 100).unwrap();
    let verifier = RepositoryVerifier::new(sandbox.path().join("trust.json"), root());
    let verified = verifier.verify_scope(&metadata, "game:drg", "stable", &host, 100).unwrap();
    assert_eq!(verified.package_records().keys().cloned().collect::<Vec<_>>(), vec![record.digest.clone()]);
    assert_eq!(verified.package_records()[&record.digest].manifest.requires, vec![requirement]);
    assert!(verified.release().provider_bindings.is_empty());
    assert!(verified.release().singleton_requirements.is_empty());

    for (name, versions) in [("unapproved", None), ("missing", Some(std::collections::BTreeSet::new())), ("incompatible", Some([2].into()))] {
        let mut unavailable = policy();
        if let Some(versions) = versions { unavailable.contracts.insert("source.network".into(), versions); }
        assert!(verify_package(&sandbox.path().join("source.consumer"), &root(), &unavailable).is_err(), "{name}");
        assert!(compose_scope(plan(&record, "game:drg", 1), vec![record.clone()], &unavailable, 9000).is_err(), "{name}");
        assert!(RepositoryVerifier::new(sandbox.path().join(format!("{name}-trust.json")), root()).verify_scope(&metadata, "game:drg", "stable", &unavailable, 100).is_err(), "{name}");
    }
}

#[test]
fn optional_missing_host_dependencies_do_not_add_module_providers() {
    let sandbox = tempfile::tempdir().unwrap();
    let mut host = policy();
    host.contracts.insert("example.assets".into(), [1].into());
    host.contracts.insert("source.network".into(), [2].into());
    for (name, contract_id, version_range) in [
        ("optional.unapproved", "source.network", "=1"),
        ("optional.incompatible", "source.network", "=2"),
        ("optional.unknown", "example.assets", "=1"),
    ] {
        let requirement = mintcat_sdk::RequiredContract { contract_id: contract_id.into(), version_range: version_range.into(), optional: true, binding_scope: Some("host".into()) };
        let record = contract_package(sandbox.path(), name, requirement.clone(), &host).unwrap();
        let metadata = compose_bundle(None, plan(&record, "game:drg", 1), vec![record.clone()], &host, &root(), &keys(), 1, 9000, 100).unwrap();
        let verified = RepositoryVerifier::new(sandbox.path().join(format!("{name}-trust.json")), root()).verify_scope(&metadata, "game:drg", "stable", &host, 100).unwrap();
        assert_eq!(verified.package_records()[&record.digest].manifest.requires, vec![requirement]);
        assert!(verified.release().provider_bindings.is_empty());
    }
    for (name, contract_id, version_range, optional) in [
        ("required.unknown", "example.assets", "=1", false),
        ("required.incompatible", "source.network", "=2", false),
        ("optional.malformed", "source.network", "invalid range", true),
    ] {
        let requirement = mintcat_sdk::RequiredContract { contract_id: contract_id.into(), version_range: version_range.into(), optional, binding_scope: Some("host".into()) };
        assert!(contract_package(sandbox.path(), name, requirement, &host).is_err(), "{name}");
    }
}

#[test]
fn host_approval_does_not_satisfy_scope_or_arbitrary_cross_scope_dependencies() {
    let sandbox = tempfile::tempdir().unwrap();
    let mut host = policy();
    host.contracts.insert("source.network".into(), [1].into());
    for (name, binding_scope, optional) in [
        ("scope.required", "scope", false),
        ("foreign.required", "game:rogue-core", false),
        ("foreign.optional", "game:rogue-core", true),
    ] {
        let requirement = mintcat_sdk::RequiredContract { contract_id: "source.network".into(), version_range: "=1".into(), optional, binding_scope: Some(binding_scope.into()) };
        let record = contract_package(sandbox.path(), name, requirement, &host).unwrap();
        assert!(compose_scope(plan(&record, "game:drg", 1), vec![record], &host, 9000).is_err(), "{name}");
    }
}

#[test]
fn host_dependency_cannot_authorize_an_unused_signed_module_binding() {
    let sandbox = tempfile::tempdir().unwrap();
    let mut host = policy();
    host.contracts.insert("source.network".into(), [1].into());
    let consumer = contract_package(sandbox.path(), "host.consumer", mintcat_sdk::RequiredContract {
        contract_id: "source.network".into(), version_range: "=1".into(), optional: false, binding_scope: Some("host".into()),
    }, &host).unwrap();
    let source = sandbox.path().join("provider-source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("data.bin"), b"resource payload").unwrap();
    let mut descriptor = manifest(b"resource payload");
    descriptor.id = "module.network".into();
    descriptor.provides.push(mintcat_sdk::ProvidedContract { contract_id: "source.network".into(), version: 1, cardinality: Some("single".into()) });
    let provider = pack(&source, descriptor, "package-key", &keys()[0], &host, &sandbox.path().join("provider")).unwrap();
    let mut exact = plan(&consumer, "game:drg", 1);
    exact.resource_dependencies.insert(consumer.digest.clone(), vec![provider.digest.clone()]);
    let available = vec![consumer, provider.clone()];
    compose_scope(exact.clone(), available.clone(), &host, 9000).unwrap();
    exact.provider_bindings.push(mintcat_sdk::ScopedProviderBinding {
        scope_id: "game:drg".into(), contract_id: "source.network".into(), contract_version: 1,
        provider_module_id: provider.manifest.id, provider_digest: provider.digest,
    });
    assert!(compose_scope(exact, available, &host, 9000).is_err());
}
