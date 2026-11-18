//! Filesystem-only smoke: packages real DRG/RC game workers with bundled resources,
//! then exercises game-only signing, independent updates and publication integrity.
use std::{collections::{BTreeMap, BTreeSet}, fs::{self, File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}, process::Command, time::{SystemTime, UNIX_EPOCH}};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::SigningKey;
use mintcat_runtime_release::*;
use mintcat_sdk::*;

fn json<T: serde::Serialize>(path: &Path, value: &T) { fs::write(path, serde_json::to_vec(value).unwrap()).unwrap(); }
fn arg(path: &Path) -> String { path.to_str().unwrap().into() }
fn invoke(cli: &Path, args: &[String]) -> std::process::Output {
    let anchor = args.windows(2).find(|pair| matches!(pair[0].as_str(), "--root" | "--policy" | "--spec" | "--key")).expect("smoke command has a fixture path");
    let cwd = Path::new(&anchor[1]).parent().unwrap();
    Command::new(cli).current_dir(cwd).arg("release").args(args).output().unwrap()
}
fn run(cli: &Path, args: Vec<String>) -> serde_json::Value {
    let result = invoke(cli, &args);
    assert!(result.status.success(), "release {:?}: {}", args, String::from_utf8_lossy(&result.stderr));
    serde_json::from_slice(&result.stdout).unwrap()
}
fn reject(cli: &Path, args: Vec<String>, reason: &str) {
    let result = invoke(cli, &args);
    assert!(!result.status.success(), "unexpected acceptance: {args:?}");
    assert!(String::from_utf8_lossy(&result.stderr).contains(reason), "wrong rejection: {}", String::from_utf8_lossy(&result.stderr));
}
fn plan(scope: &str, record: &PackageRecord, sequence: u64) -> ScopePlan {
    ScopePlan { scope_id: scope.into(), release_id: format!("{scope}-smoke-{sequence}"), sequence, channel: "stable".into(), target: record.manifest.target.clone(), min_host_api: "1".into(),
        roots: vec![record.digest.clone()], provider_bindings: record.manifest.provides.iter().map(|contract| ScopedProviderBinding { scope_id: scope.into(), contract_id: contract.contract_id.clone(), contract_version: contract.version, provider_module_id: record.manifest.id.clone(), provider_digest: record.digest.clone() }).collect(), singleton_requirements: vec![],
        resource_dependencies: BTreeMap::new(), cohort: CohortPolicy { salt: format!("{scope}-local-cohort"), basis_points: 2500 } }
}
fn test_patch_archive() -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    archive.start_file("TEST-ONLY-NOT-INSTALLABLE.txt", zip::write::SimpleFileOptions::default()).unwrap();
    archive.write_all(b"Isolated signed package transport fixture; not real game integration assets.").unwrap();
    archive.finish().unwrap().into_inner()
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert!((3..=4).contains(&args.len()), "usage: local_smoke <mintcat-module> <DRG worker> <RC worker> [real DRG patch archive]");
    let cli = fs::canonicalize(&args[0]).unwrap();
    let drg = fs::canonicalize(&args[1]).unwrap();
    let rc = fs::canonicalize(&args[2]).unwrap();
    let drg_patch = args.get(3).map(|path| fs::read(path).expect("explicit real DRG patch archive must be readable"));
    let sandbox = tempfile::tempdir().unwrap();
    let base = sandbox.path();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let expires = now + 3600;
    let mut key_paths = Vec::new();
    let mut signing_keys = Vec::new();
    for n in 0..2 {
        let mut seed = [0; 32];
        File::open("/dev/urandom").unwrap().read_exact(&mut seed).unwrap();
        let path = base.join(format!("key-{n}"));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        options.open(&path).unwrap().write_all(BASE64.encode(seed).as_bytes()).unwrap();
        signing_keys.push(SigningKey::from_bytes(&seed));
        key_paths.push(path);
    }
    let public = signing_keys.iter().map(|key| (key_id(&key.verifying_key()), encode_public_key(&key.verifying_key()))).collect::<BTreeMap<_, _>>();
    let spec = RootMetadata {
        header: MetadataHeader { kind: "root".into(), schema_version: 1, version: 1, expires },
        keys: public.clone(),
        roles: ["root", "targets", "snapshot", "timestamp"].into_iter().map(|role| (role.into(), Role { key_ids: public.keys().cloned().collect::<BTreeSet<_>>(), threshold: 2 })).collect(),
        publishers: BTreeMap::from([("smoke.local".into(), BTreeMap::from([("package-key".into(), encode_public_key(&signing_keys[0].verifying_key()))]))]),
    };
    let spec_path = base.join("root-spec.json"); json(&spec_path, &spec);
    let root_path = base.join("root.json");
    run(&cli, vec!["sign-root".into(), "--spec".into(), arg(&spec_path), "--key".into(), arg(&key_paths[0]), "--key".into(), arg(&key_paths[1]), "--output".into(), arg(&root_path)]);
    let target = target_lexicon::HOST.to_string();
    let payloads = vec![
        ("drg", "worker", fs::read(&drg).unwrap(), true),
        ("rc", "worker", fs::read(&rc).unwrap(), true),
    ];
    let mut manifests = Vec::new();
    let mut sources = Vec::new();
    for (name, entry, bytes, executable) in payloads {
        let source = base.join(format!("{name}-source")); fs::create_dir(&source).unwrap(); fs::create_dir_all(source.join(entry).parent().unwrap()).unwrap(); fs::write(source.join(entry), &bytes).unwrap();
        let file = ModuleFile { path: entry.into(), size: bytes.len() as u64, sha256: sha256(&bytes), executable };
        let mut manifest = match name {
            "drg" => mintcat_game_drg::module_manifest(&target, "1", file),
            "rc" => mintcat_game_rogue_core::module_manifest(&target, "1", file),
            _ => unreachable!(),
        };
        manifest.publisher = "smoke.local".into();
        let bytes = b"isolated bundled game resource";
        fs::write(source.join("resource.dat"), bytes).unwrap();
        manifest.components.push(ModuleComponent { kind: "resource".into(), entry: "resource.dat".into(), runtime: "data".into(), state_model: "stateless".into() });
        manifest.files.push(ModuleFile { path: "resource.dat".into(), size: bytes.len() as u64, sha256: sha256(bytes), executable: false });
        let fixture = test_patch_archive();
        let bytes = if name == "drg" { drg_patch.as_deref().unwrap_or(&fixture) } else { &fixture };
        let resource_id = if name == "drg" { "resource.drg.patch" } else { "resource.rogue-core.patch" };
        let entry = format!("resources/{name}-patch.zip");
        fs::create_dir(source.join("resources")).unwrap();
        fs::write(source.join(&entry), bytes).unwrap();
        manifest.components.push(ModuleComponent { kind: "resource".into(), entry: entry.clone(), runtime: "data".into(), state_model: "stateless".into() });
        manifest.files.push(ModuleFile { path: entry.clone(), size: bytes.len() as u64, sha256: sha256(bytes), executable: false });
        manifest.contributes["bundledResources"] = serde_json::json!([{ "resourceId": resource_id, "version": "1.0.0", "archivePath": entry, "deploymentBindings": [] }]);
        let manifest_path = base.join(format!("{name}-manifest.json")); json(&manifest_path, &manifest);
        manifests.push(manifest); sources.push((name, source, manifest_path));
    }
    let mut policy = VerificationPolicy { target, host_api: "1".into(), max_data_schema: 1, permissions: vec![], contracts: BTreeMap::new() };
    for manifest in &manifests {
        for permission in &manifest.permissions { if !policy.permissions.contains(permission) { policy.permissions.push(permission.clone()); } }
        for contract in &manifest.provides { policy.contracts.entry(contract.contract_id.clone()).or_default().insert(contract.version); }
    }
    let policy_path = base.join("policy.json"); json(&policy_path, &policy);
    let mut packages = Vec::<PathBuf>::new();
    let mut records = Vec::<PackageRecord>::new();
    for (index, (name, source, manifest_path)) in sources.iter().enumerate() {
        let package = base.join(format!("{name}-package"));
        let command = vec!["pack".into(), "--source".into(), arg(source), "--manifest".into(), arg(manifest_path), "--policy".into(), arg(&policy_path), "--key".into(), arg(&key_paths[0]), "--key-id".into(), "package-key".into(), "--output".into(), arg(&package)];
        let mut missing_resource = manifests[index].clone();
        missing_resource.contributes["bundledResources"] = serde_json::json!([]);
        let missing_manifest = base.join(format!("{name}-missing-resource.json")); json(&missing_manifest, &missing_resource);
        reject(&cli, vec!["pack".into(), "--source".into(), arg(source), "--manifest".into(), arg(&missing_manifest), "--policy".into(), arg(&policy_path), "--key".into(), arg(&key_paths[0]), "--key-id".into(), "package-key".into(), "--output".into(), arg(&base.join(format!("{name}-missing-resource-package")))], "required game resource");
        let result = run(&cli, command);
        records.push(serde_json::from_value(result).unwrap());
        run(&cli, vec!["verify".into(), "--root".into(), arg(&root_path), "--policy".into(), arg(&policy_path), "--package".into(), arg(&package)]);
        packages.push(package);
    }
    let repository_path = base.join("repository");
    let client_state = PathBuf::from("client-trust.json");
    let mut previous: Option<PathBuf> = None;
    let mut first = None;
    let mut rc_targets = None;
    for (scope, index, sequence, version) in [("game:drg", 0, 1, 1), ("game:rogue-core", 1, 1, 2), ("game:drg", 0, 2, 3)] {
        let plan_path = base.join(format!("plan-{version}.json")); json(&plan_path, &plan(scope, &records[index], sequence));
        let bundle_path = base.join(format!("bundle-{version}.json"));
        let mut command = vec!["compose".into(), "--root".into(), arg(&root_path), "--policy".into(), arg(&policy_path), "--plan".into(), arg(&plan_path), "--metadata-version".into(), version.to_string(), "--expires".into(), expires.to_string(), "--output".into(), arg(&bundle_path)];
        for key in &key_paths { command.extend(["--key".into(), arg(key)]); }
        command.extend(["--package".into(), arg(&packages[index])]);
        if let Some(path) = &previous { command.extend(["--previous".into(), arg(path)]); }
        run(&cli, command);
        let bundle: RepositoryBundle = read_json(&bundle_path).unwrap();
        if version == 1 {
            assert!(bundle.targets.values().all(|targets| !targets.signed.packages.values().any(|package| package.manifest.id == manifests[1].id)));
            first = Some(bundle_path.clone());
        }
        let rc_name = scope_metadata_key("game:rogue-core", "stable", &policy.target).unwrap();
        if version == 2 { rc_targets = Some(bundle.targets[&rc_name].clone()); }
        if version == 3 { assert_eq!(rc_targets.as_ref().unwrap(), &bundle.targets[&rc_name]); }
        let mut command = vec!["publish".into(), "--repository".into(), arg(&repository_path), "--root".into(), arg(&root_path), "--policy".into(), arg(&policy_path), "--bundle".into(), arg(&bundle_path)];
        let selected_name = scope_metadata_key(scope, "stable", &policy.target).unwrap();
        let selected = bundle.targets[&selected_name].signed.packages.keys().map(String::as_str).collect::<BTreeSet<_>>();
        for (package, record) in packages.iter().zip(&records) { if selected.contains(record.digest.as_str()) { command.extend(["--package".into(), arg(package)]); } }
        run(&cli, command);
        run(&cli, vec!["verify".into(), "--root".into(), arg(&root_path), "--policy".into(), arg(&policy_path), "--bundle".into(), arg(&bundle_path), "--state".into(), arg(&client_state)]);
        previous = Some(bundle_path);
    }
    reject(&cli, vec!["verify".into(), "--root".into(), arg(&root_path), "--policy".into(), arg(&policy_path), "--bundle".into(), arg(&first.unwrap()), "--state".into(), arg(&client_state)], "rollback");
    // A second validly signed payload under the same identity must not upload.
    let source_index = 0;
    fs::write(sources[source_index].1.join("resource.dat"), b"different signed payload").unwrap();
    let mut collision = manifests[source_index].clone(); collision.files[1].size = b"different signed payload".len() as u64; collision.files[1].sha256 = sha256(b"different signed payload");
    json(&sources[source_index].2, &collision);
    let collision_path = base.join("collision-package");
    run(&cli, vec!["pack".into(), "--source".into(), arg(&sources[source_index].1), "--manifest".into(), arg(&sources[source_index].2), "--policy".into(), arg(&policy_path), "--key".into(), arg(&key_paths[0]), "--key-id".into(), "package-key".into(), "--output".into(), arg(&collision_path)]);
    reject(&cli, vec!["publish".into(), "--repository".into(), arg(&repository_path), "--root".into(), arg(&root_path), "--policy".into(), arg(&policy_path), "--package".into(), arg(&collision_path)], "collision");
    fs::write(packages[0].join("payload/resource.dat"), b"tampered").unwrap();
    reject(&cli, vec!["verify".into(), "--root".into(), arg(&root_path), "--policy".into(), arg(&policy_path), "--package".into(), arg(&packages[0])], "bytes");
    println!("PASS: actual game workers with explicitly TEST-ONLY archives; both missing-required-resource rejections; two-key thresholds; independent game sequences; local publication, persisted rollback, length and collision rejection. This exercises signing/transport only, not real game installation; no remote pointers touched.");
}
