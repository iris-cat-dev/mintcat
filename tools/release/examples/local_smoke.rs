//! Filesystem-only smoke: packages actual built DRG/RC worker binaries, UI,
//! resource and native payloads, then exercises `mintcat-module release`.
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
fn base_manifest(id: &str, file: ModuleFile, kind: &str, runtime: &str, target: &str) -> ModuleManifest {
    ModuleManifest { schema_version: 1, id: id.into(), version: "1.0.0".into(), publisher: "smoke.local".into(), target: target.into(), host_api: "1".into(),
        components: vec![ModuleComponent { kind: kind.into(), entry: file.path.clone(), runtime: runtime.into(), state_model: "stateless".into() }],
        provides: vec![], requires: vec![], permissions: vec![], contributes: serde_json::json!({}),
        state: ModuleState { schema_version: 1, readable_range: "1".into(), writable_range: "1".into(), rollback_before_write_only: Some(true) },
        activation: ModuleActivation { trigger: "on-demand".into(), drain_policy: "finish-requests".into() }, files: vec![file] }
}
fn plan(scope: &str, record: &PackageRecord, resources: &[PackageRecord], sequence: u64) -> ScopePlan {
    ScopePlan { scope_id: scope.into(), release_id: format!("{scope}-smoke-{sequence}"), sequence, channel: "stable".into(), target: record.manifest.target.clone(), min_host_api: "1".into(),
        roots: vec![record.digest.clone()], provider_bindings: vec![], singleton_requirements: vec![],
        resource_dependencies: if resources.is_empty() { BTreeMap::new() } else { BTreeMap::from([(record.digest.clone(), resources.iter().map(|r| r.digest.clone()).collect())]) },
        cohort: CohortPolicy { salt: format!("{scope}-local-cohort"), basis_points: 2500 } }
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(args.len(), 3, "usage: local_smoke <mintcat-module> <DRG worker> <RC worker>");
    let cli = fs::canonicalize(&args[0]).unwrap();
    let drg = fs::canonicalize(&args[1]).unwrap();
    let rc = fs::canonicalize(&args[2]).unwrap();
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
        ("ui", "ui/index.js", b"export async function mountPage(root) { const button = document.createElement('button'); button.textContent = 'Signed local module'; const click = () => { button.textContent = 'Verified UI'; }; button.addEventListener('click', click); root.append(button); return {dispose() { button.removeEventListener('click', click); button.remove(); }}; }".to_vec(), false),
        ("resource", "resource.dat", b"isolated local release resource".to_vec(), false),
        ("native", "mintcat-module", fs::read(&cli).unwrap(), true),
    ];
    let mut manifests = Vec::new();
    let mut sources = Vec::new();
    for (name, entry, bytes, executable) in payloads {
        let source = base.join(format!("{name}-source")); fs::create_dir(&source).unwrap(); fs::create_dir_all(source.join(entry).parent().unwrap()).unwrap(); fs::write(source.join(entry), &bytes).unwrap();
        let file = ModuleFile { path: entry.into(), size: bytes.len() as u64, sha256: sha256(&bytes), executable };
        let mut manifest = match name {
            "drg" => mintcat_game_drg::module_manifest(&target, "1", file),
            "rc" => mintcat_game_rogue_core::module_manifest(&target, "1", file),
            "ui" => base_manifest("smoke.ui", file, "ui", "webview", &target),
            "resource" => base_manifest("smoke.resource", file, "resource", "data", &target),
            "native" => base_manifest("smoke.native", file, "native", "payload", &target),
            _ => unreachable!(),
        };
        manifest.publisher = "smoke.local".into();
        if name == "ui" {
            let descriptor = serde_json::json!({"schemaVersion":1,"runtimeVersion":1,"moduleId":"smoke.ui","entry":"ui/index.js","css":[],"pages":[{"id":"smoke.ui/page","route":"/smoke.ui/local","title":"Signed local module","export":"mountPage","stateVersion":1}],"navigation":[{"id":"smoke.ui/nav","pageId":"smoke.ui/page","order":1}],"commands":[],"settings":[],"games":[]});
            let bytes = serde_json::to_vec(&descriptor).unwrap();
            fs::write(source.join("ui/contributions.json"), &bytes).unwrap();
            manifest.files.push(ModuleFile { path: "ui/contributions.json".into(), size: bytes.len() as u64, sha256: sha256(&bytes), executable: false });
        }
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
    for (name, source, manifest_path) in &sources {
        let package = base.join(format!("{name}-package"));
        let result = run(&cli, vec!["pack".into(), "--source".into(), arg(source), "--manifest".into(), arg(manifest_path), "--policy".into(), arg(&policy_path), "--key".into(), arg(&key_paths[0]), "--key-id".into(), "package-key".into(), "--output".into(), arg(&package)]);
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
        let plan_path = base.join(format!("plan-{version}.json")); json(&plan_path, &plan(scope, &records[index], &records[2..], sequence));
        let bundle_path = base.join(format!("bundle-{version}.json"));
        let mut command = vec!["compose".into(), "--root".into(), arg(&root_path), "--policy".into(), arg(&policy_path), "--plan".into(), arg(&plan_path), "--metadata-version".into(), version.to_string(), "--expires".into(), expires.to_string(), "--output".into(), arg(&bundle_path)];
        for key in &key_paths { command.extend(["--key".into(), arg(key)]); }
        for package in &packages { command.extend(["--package".into(), arg(package)]); }
        if let Some(path) = &previous { command.extend(["--previous".into(), arg(path)]); }
        run(&cli, command);
        let bundle: RepositoryBundle = read_json(&bundle_path).unwrap();
        if version == 1 { assert!(bundle.targets.values().all(|t| !t.signed.packages.values().any(|p| p.manifest.id == manifests[1].id))); first = Some(bundle_path.clone()); }
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
    fs::write(sources[3].1.join("resource.dat"), b"different signed payload").unwrap();
    let mut collision = manifests[3].clone(); collision.files[0].size = b"different signed payload".len() as u64; collision.files[0].sha256 = sha256(b"different signed payload");
    json(&sources[3].2, &collision);
    let collision_path = base.join("collision-package");
    run(&cli, vec!["pack".into(), "--source".into(), arg(&sources[3].1), "--manifest".into(), arg(&sources[3].2), "--policy".into(), arg(&policy_path), "--key".into(), arg(&key_paths[0]), "--key-id".into(), "package-key".into(), "--output".into(), arg(&collision_path)]);
    reject(&cli, vec!["publish".into(), "--repository".into(), arg(&repository_path), "--root".into(), arg(&root_path), "--policy".into(), arg(&policy_path), "--package".into(), arg(&collision_path)], "collision");
    fs::write(packages[3].join("payload/resource.dat"), b"tampered").unwrap();
    reject(&cli, vec!["verify".into(), "--root".into(), arg(&root_path), "--policy".into(), arg(&policy_path), "--package".into(), arg(&packages[3])], "bytes");
    println!("PASS: real DRG/RC worker, UI, resource, native packaging; 2-key thresholds; local publication; independent game sequences; persisted rollback, length and collision rejection. No remote pointers touched.");
}
