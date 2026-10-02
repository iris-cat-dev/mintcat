use std::{fs, path::{Path, PathBuf}, process::Command, time::Duration, collections::BTreeMap};
use mintcat_runtime_release::{key_id, read_json, read_signing_key, sha256, VerificationPolicy};
use mintcat_runtime_artifacts::{ArtifactStore, TrustStore, module_signing_digest};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
use mintcat_runtime_supervisor::{Invocation, ModuleSupervisor};
use mintcat_sdk::{ModuleManifest, ModuleFile, ScopedProviderBinding};
use serde::{Serialize, Deserialize};
use serde_json::json;
type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;
#[derive(Serialize, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct Project {
    manifest: ModuleManifest,
    policy: VerificationPolicy,
    worker_binary: String,
    /// Optional real TypeScript UI source; built by the single shared SDK UI builder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ui_source: Option<PathBuf>,
}
fn put(path: &Path, value: &impl Serialize) -> Result<()> { fs::write(path, serde_json::to_vec_pretty(value)?)?; Ok(()) }
fn host_target(target: &str) -> Result<()> {
    let triple = target.parse::<target_lexicon::Triple>().map_err(|error| error.to_string())?;
    if triple.to_string() != target || triple.operating_system == target_lexicon::OperatingSystem::Unknown || triple.architecture == target_lexicon::Architecture::Unknown { return Err("a canonical complete target triple is required".into()); }
    Ok(())
}
pub fn new(directory: &Path, id: &str, target: &str) -> Result<()> {
    host_target(target)?;
    if id.len() < 2 || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-".contains(&b)) || !id.as_bytes()[0].is_ascii_alphanumeric() { return Err("module id must use lowercase letters, digits, dot or hyphen".into()); }
    fs::create_dir(directory)?;
    fs::create_dir(directory.join("src"))?;
    fs::create_dir_all(directory.join("payload/ui"))?;
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/sdk-rust").canonicalize()?;
    fs::write(directory.join("Cargo.toml"), format!("[package]\nname = \"mintcat-example-worker\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n[dependencies]\nmintcat-sdk = {{ path = {} }}\ntokio = {{ version = \"1\", features = [\"rt-multi-thread\", \"macros\", \"io-std\", \"io-util\"] }}\nserde_json = \"1\"\n", serde_json::to_string(&sdk.to_string_lossy())?))?;
    fs::write(directory.join("src/main.rs"), "use mintcat_sdk::{run_worker, WorkerIdentity};\n#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {\n run_worker(WorkerIdentity::from_env()?, |request| async move {\n  Ok(serde_json::json!({\"example\": true, \"method\": request.method, \"input\": request.params}))\n }).await?; Ok(())\n}\n")?;
    fs::write(directory.join("payload/ui/index.js"), "export async function mountExample(root, context) {\n const text = context.kit.element('p', 'Runnable local module example, not a game implementation.');\n const output = context.kit.element('pre');\n const button = context.kit.button('Invoke real worker', context.guard(async () => { output.textContent = JSON.stringify(await context.request('example', {from:'signed-ui'}), null, 2); }));\n root.append(text,button,output); return { dispose() { text.remove(); button.remove(); output.remove(); } };\n}\n")?;
    let worker = if target.ends_with("windows-msvc") || target.ends_with("windows-gnu") { "worker.exe" } else { "worker" };
    fs::write(directory.join("payload/example.txt"), "Local runnable example resource; not a game payload.\n")?;
    put(&directory.join("payload/ui/contributions.json"), &json!({"schemaVersion":1,"runtimeVersion":1,"moduleId":id,"entry":"ui/index.js","css":[],"pages":[{"id":format!("{id}/example"),"route":format!("/{id}/example"),"title":"Local example","export":"mountExample","stateVersion":1}],"navigation":[],"commands":[],"settings":[],"games":[]}))?;
    let manifest: ModuleManifest = serde_json::from_value(json!({"schemaVersion":1,"id":id,"version":"0.1.0","publisher":"local-example","target":target,"hostApi":"1","components":[{"kind":"service","entry":worker,"runtime":"worker","stateModel":"stateless"},{"kind":"ui","entry":"ui/index.js","runtime":"webview","stateModel":"stateless"},{"kind":"resource","entry":"example.txt","runtime":"data","stateModel":"stateless"}],"provides":[{"contractId":format!("{id}.example"),"version":1,"cardinality":"single"}],"requires":[],"permissions":[],"contributes":{"origin":"production"},"state":{"schemaVersion":1,"readableRange":"1","writableRange":"1"},"activation":{"trigger":"on-demand","drainPolicy":"finish-requests"},"files":[]}))?;
    let policy: VerificationPolicy = serde_json::from_value(json!({"target":target,"hostApi":"1","maxDataSchema":1,"permissions":[],"contracts":{(format!("{id}.example")):[1]}}))?;
    put(&directory.join("module-project.json"), &Project { manifest, policy, worker_binary: "mintcat-example-worker".into(), ui_source: None })?;
    println!("Created runnable worker/UI example at {} for {target}; this is not a game implementation", directory.display());
    Ok(())
}
fn files(root: &Path, dir: &Path, out: &mut Vec<ModuleFile>, worker: &str) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?; let path = entry.path(); let kind = entry.file_type()?;
        if kind.is_symlink() { return Err(format!("symlink payload rejected: {}", path.display()).into()); }
        if kind.is_dir() { files(root, &path, out, worker)?; }
        else if kind.is_file() {
            let relative = path.strip_prefix(root)?.to_str().ok_or("non-UTF8 payload path")?.replace('\\', "/");
            let bytes = fs::read(&path)?;
            out.push(ModuleFile { executable: relative == worker, path: relative, size: bytes.len() as u64, sha256: sha256(&bytes) });
        } else { return Err("payload must contain only regular files".into()); }
    }
    Ok(())
}
fn build(directory: &Path) -> Result<(Project, PathBuf)> {
    let mut project: Project = read_json(&directory.join("module-project.json"))?;
    host_target(&project.manifest.target)?;
    if project.worker_binary.is_empty() || !project.worker_binary.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)) { return Err("workerBinary must name an actual Cargo binary".into()); }
    let status = Command::new("cargo").args(["build", "--bin", &project.worker_binary, "--manifest-path"]).arg(directory.join("Cargo.toml")).args(["--target", &project.manifest.target, "--target-dir"]).arg(directory.join("target")).status()?;
    if !status.success() { return Err("incremental worker build failed; diagnostics are above".into()); }
    let worker = project.manifest.components.iter().find(|c| c.kind == "service" && c.runtime == "worker").ok_or("project requires a worker component")?.entry.clone();
    if worker.contains('/') || worker.contains('\\') || worker == ".." { return Err("worker entry must be a simple filename".into()); }
    let executable = format!("{}{}", project.worker_binary, if worker.ends_with(".exe") { ".exe" } else { "" });
    let payload = directory.join("payload");
    fs::copy(directory.join("target").join(&project.manifest.target).join("debug").join(executable), payload.join(&worker))?;
    if let Some(source) = &project.ui_source {
        let source = directory.join(source).canonicalize()?;
        let ui = payload.join("ui");
        if source.starts_with(payload.canonicalize()?) { return Err("uiSource must be outside generated payload; never overwrite UI source during staging".into()); }
        let staged = directory.join("target").join(format!("ui-build-{}", uuid::Uuid::new_v4()));
        let builder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/sdk-ui/scripts/build.mjs");
        let status = Command::new("node").arg(builder).arg("--source").arg(source).arg("--output").arg(&staged).stdout(std::process::Stdio::from(std::io::stderr())).status()?;
        if !status.success() {
            if staged.exists() { fs::remove_dir_all(&staged)?; }
            return Err("shared UI build failed; diagnostics are above".into());
        }
        let previous = directory.join("target").join(format!("ui-old-{}", uuid::Uuid::new_v4()));
        if ui.exists() { fs::rename(&ui, &previous)?; }
        if let Err(error) = fs::rename(&staged, &ui) {
            if previous.exists() { fs::rename(&previous, &ui)?; }
            if staged.exists() { fs::remove_dir_all(&staged)?; }
            return Err(error.into());
        }
        if previous.exists() { fs::remove_dir_all(previous)?; }
    }
    project.manifest.files.clear();
    files(&payload, &payload, &mut project.manifest.files, &worker)?;
    project.manifest.files.sort_by(|a,b| a.path.cmp(&b.path));
    project.policy.validate(&project.manifest)?;
    module_signing_digest(&payload, &project.manifest)?;
    mintcat_runtime_release::validate_component_payloads(&payload, &project.manifest)?;
    Ok((project, payload))
}
pub fn check(directory: &Path) -> Result<ModuleManifest> {
    let (project, _) = build(directory)?;
    println!("{}", serde_json::to_string_pretty(&json!({"module":project.manifest,"policy":project.policy,"processes":[],"status":"validated-build-and-payload"}))?);
    Ok(project.manifest)
}
pub fn pack(directory: &Path, key: &Path, output: &Path) -> Result<()> {
    let (project, payload) = build(directory)?;
    mintcat_runtime_artifacts::validate_production_origin(&project.manifest)?;
    let key = read_signing_key(key)?;
    let record = mintcat_runtime_release::pack(&payload, project.manifest, &key_id(&key.verifying_key()), &key, &project.policy, output)?;
    println!("{}", serde_json::to_string_pretty(&record)?); Ok(())
}
fn fingerprint(directory: &Path) -> Result<String> {
    fn visit(dir: &Path, out: &mut BTreeMap<PathBuf, String>) -> Result<()> {
        for entry in fs::read_dir(dir)? { let entry = entry?; let name = entry.file_name();
            if ["target", "payload", ".git"].iter().any(|skip| name == *skip) { continue; }
            if entry.file_type()?.is_symlink() { return Err("source symlinks are not supported".into()); }
            if entry.file_type()?.is_dir() { visit(&entry.path(), out)?; } else { out.insert(entry.path(), sha256(&fs::read(entry.path())?)); }
        } Ok(())
    }
    let mut source = BTreeMap::new(); visit(directory, &mut source)?;
    let project: Project = read_json(&directory.join("module-project.json"))?;
    visit(&directory.join("payload"), &mut source)?;
    for component in project.manifest.components.iter().filter(|component| component.runtime == "worker") { source.remove(&directory.join("payload").join(&component.entry)); }
    if let Some(ui_source) = &project.ui_source {
        let generated_ui = directory.join("payload/ui");
        source.retain(|path, _| !path.starts_with(&generated_ui));
        visit(&directory.join(ui_source), &mut source)?;
    }
    Ok(sha256(&serde_json::to_vec(&source)?))
}
pub async fn dev(directory: &Path, root: &Path, key_path: &Path, dependencies: &[PathBuf], trust_root: Option<&Path>, method: &str, params: Option<&Path>, once: bool) -> Result<()> {
    let shutdown = crate::shutdown_signal()?;
    tokio::pin!(shutdown);
    // Prime the non-Unix Ctrl-C future before any synchronous build work, without dropping it.
    tokio::select! { biased; result = &mut shutdown => { result?; return Ok(()); }, _ = std::future::ready(()) => {} }
    let directory = directory.canonicalize()?;
    fs::create_dir_all(root)?; let root = root.canonicalize()?;
    if root.starts_with(&directory) || directory.starts_with(&root) { return Err("dev root must be separate from the source project".into()); }
    let key = read_signing_key(key_path)?; let identity = key_id(&key.verifying_key());
    let marker = root.join(".module-dev.json");
    if marker.exists() { let old: serde_json::Value = read_json(&marker)?; if old != json!({"source":directory,"keyId":identity}) { return Err("dev root belongs to a different source or signing key".into()); } }
    else {
        if fs::read_dir(&root)?.next().is_some() { return Err("new dev root must be empty (never reuse a production store)".into()); }
        put(&marker, &json!({"source":directory,"keyId":identity}))?;
    }
    let layout = RuntimePaths::new(root.join("runtime-data"))?.initialize(RuntimeEnvironment::Development)?;
    let (surface, server) = crate::surface::start(&root).await?;
    let mut previous: Option<ModuleSupervisor> = None;
    let mut pending_generation: Option<ModuleSupervisor> = None;
    let mut previous_stateless = true;
    let mut current_fingerprint = String::new();
    let result: Result<()> = async {
    loop {
        let next = fingerprint(&directory)?;
        if next != current_fingerprint {
            let attempt: Result<ModuleSupervisor> = async {
                let (mut project, payload) = build(&directory)?;
                project.manifest.contributes.as_object_mut().ok_or("contributes must be an object")?.insert("origin".into(), json!("development"));
                let generation = uuid::Uuid::new_v4().to_string();
                let package_path = root.join("packages").join(&generation);
                let record = mintcat_runtime_release::pack(&payload, project.manifest, &identity, &key, &project.policy, &package_path)?;
                let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
                let pinned = trust_root.map(|path| mintcat_runtime_release::load_trust_root(path, time)).transpose()?;
                let mut trust = match &pinned { Some(root) => root.artifact_trust_store()?, None => TrustStore::default() };
                trust.trust_development_key(&record.manifest.publisher, &identity, key.verifying_key());
                let store = ArtifactStore::from_layout(&layout, RuntimeEnvironment::Development, &record.manifest.target, &record.manifest.host_api, trust)?;
                let installed = store.publish_module(&package_path.join("payload"), &record.manifest, &record.signature)?;
                let supervisor = ModuleSupervisor::new(store, &generation, &layout.runtime_dir)?;
                pending_generation = Some(supervisor.clone());
                let provided = record.manifest.provides.first().ok_or("dev worker must provide a contract")?;
                let scope = format!("dev:{}", record.manifest.id);
                let mut dependency_records = Vec::new();
                for package in dependencies {
                    let pinned = pinned.as_ref().ok_or("dependency packages require explicit --trust-root")?;
                    let dependency = mintcat_runtime_release::verify_package(package, pinned, &project.policy)?;
                    supervisor.artifacts().publish_module(&package.join("payload"), &dependency.manifest, &dependency.signature)?;
                    dependency_records.push(dependency);
                }
                let stateless = std::iter::once(&record.manifest).chain(dependency_records.iter().map(|record| &record.manifest)).all(|manifest| manifest.components.iter().filter(|component| component.runtime == "worker").all(|component| component.state_model == "stateless"));
                // A single-writer worker must never overlap another generation against the same dev state.
                if !stateless || !previous_stateless {
                    if let Some(old) = previous.take() {
                        *surface.write().map_err(|_| "dev UI generation lock poisoned")? = None;
                        old.shutdown_all().await?;
                    }
                }
                let mut pending = dependency_records.iter().filter(|dependency| dependency.manifest.components.iter().any(|component| component.runtime == "worker")).collect::<Vec<_>>();
                let mut bindings = std::collections::BTreeSet::new();
                for dependency in &pending {
                    for contract in &dependency.manifest.provides {
                        if !bindings.insert((contract.contract_id.clone(),contract.version)) || record.manifest.provides.iter().any(|provided| provided.contract_id == contract.contract_id && provided.version == contract.version) { return Err("ambiguous dependency provider override rejected".into()); }
                    }
                }
                while !pending.is_empty() {
                    let mut activated = Vec::new();
                    for (index, dependency) in pending.iter().enumerate() {
                        let satisfied = dependency.manifest.requires.iter().filter(|requirement| !requirement.optional).all(|requirement| dependency_records.iter().filter(|record| record.manifest.components.iter().any(|component| component.runtime == "worker") && !pending.iter().any(|pending| pending.digest == record.digest)).any(|record| record.manifest.provides.iter().any(|provided| provided.contract_id == requirement.contract_id && mintcat_runtime_release::version_matches(&requirement.version_range, provided.version).unwrap_or(false))));
                        if !satisfied { continue; }
                        for provided in &dependency.manifest.provides {
                            supervisor.activate(ScopedProviderBinding { scope_id:scope.clone(), contract_id:provided.contract_id.clone(), contract_version:provided.version, provider_module_id:dependency.manifest.id.clone(), provider_digest:dependency.digest.clone() }).await?;
                        }
                        activated.push(index);
                    }
                    if activated.is_empty() { supervisor.shutdown_all().await?; return Err("dependency cycle or missing worker provider; signed dependencies cannot be activated".into()); }
                    for index in activated.into_iter().rev() { pending.remove(index); }
                }
                supervisor.activate(ScopedProviderBinding { scope_id:scope.clone(), contract_id:provided.contract_id.clone(), contract_version:provided.version, provider_module_id:record.manifest.id.clone(), provider_digest:record.digest.clone() }).await?;
                let params = match params { Some(path) => read_json(path)?, None => json!({"generation":generation}) };
                let result = supervisor.invoke(Invocation { scope_id:&scope, contract_id:&provided.contract_id, contract_version:provided.version, method, params, grants:&[], deadline:Duration::from_secs(5) }).await;
                let result = match result { Ok(value) => value, Err(error) => { supervisor.shutdown_all().await?; return Err(error.into()); } };
                // Admission switches to the healthy generation before stateless old requests drain.
                *surface.write().map_err(|_| "dev UI generation lock poisoned")? = Some(crate::surface::Generation { id:generation.clone(),supervisor:supervisor.clone(),payload:installed.root,manifest:std::sync::Arc::new(record.manifest.clone()),scope,contract:provided.contract_id.clone(),version:provided.version });
                if let Some(old) = previous.take() {
                    if let Err(error) = old.shutdown_all().await {
                        *surface.write().map_err(|_| "dev UI generation lock poisoned")? = None;
                        supervisor.shutdown_all().await?;
                        return Err(error.into());
                    }
                }
                previous_stateless = stateless;
                println!("{}", serde_json::to_string_pretty(&json!({"generation":generation,"module":record.manifest,"digest":record.digest,"dependencies":dependency_records,"requirements":record.manifest.requires,"result":result,"processes":supervisor.snapshots().iter().map(|p| json!({"instanceId":p.instance_id,"moduleId":p.module_id,"version":p.module_version,"digest":p.module_digest,"lifecycle":format!("{:?}",p.lifecycle),"activeRequests":p.active_requests,"log":p.log_path})).collect::<Vec<_>>() }))?);
                Ok(supervisor)
            }.await;
            current_fingerprint = if attempt.is_ok() { fingerprint(&directory)? } else { next };
            match attempt {
                Ok(supervisor) => { previous = Some(supervisor); pending_generation = None; }
                Err(error) => {
                    if let Some(supervisor) = pending_generation.take() { supervisor.shutdown_all().await?; }
                    eprintln!("generation rejected: {error}; prior generation {}", if previous.is_some() { "retained" } else { "not running" });
                    if once || previous.is_none() { return Err(error); }
                }
            }
        }
        if once { return Ok(()); }
        tokio::select! { biased; result = &mut shutdown => { result?; return Ok(()); }, _ = tokio::time::sleep(Duration::from_millis(500)) => {} }
    }
    }.await;
    // Closing HTTP admission and finishing its owned requests precedes framed worker shutdown.
    let mut failure = result.err();
    if let Err(error) = server.shutdown().await { failure.get_or_insert(error.into()); }
    match surface.write() {
        Ok(mut generation) => *generation = None,
        Err(_) => { failure.get_or_insert_with(|| "dev UI generation lock poisoned".into()); }
    }
    let had_workers = previous.is_some() || pending_generation.is_some();
    for supervisor in previous.into_iter().chain(pending_generation) {
        if let Err(error) = supervisor.shutdown_all().await { failure.get_or_insert(error.into()); }
    }
    match failure {
        Some(error) => Err(error),
        None => { if had_workers { println!("framed drain complete"); } Ok(()) }
    }
}
