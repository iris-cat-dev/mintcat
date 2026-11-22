use std::{collections::BTreeSet, fs, io::Read, path::{Path, PathBuf}, process::Command};
use mintcat_runtime_artifacts::module_signing_digest;
use mintcat_runtime_release::{key_id, read_json, read_signing_key, validate_game_manifest, VerificationPolicy};
use mintcat_sdk::{ModuleFile, ModuleManifest};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Project {
    manifest: ModuleManifest,
    policy: VerificationPolicy,
    worker_binary: String,
}

fn executable_paths(manifest: &mut ModuleManifest, worker: &str) -> Result<BTreeSet<String>> {
    let mut paths: BTreeSet<String> = manifest.files.iter().filter(|file| file.executable).map(|file| file.path.clone()).collect();
    paths.insert(worker.into());
    if let Some(resources) = manifest.contributes.get("bundledResources") {
        for resource in resources.as_array().ok_or("bundledResources must be an array")? {
            if let Some(bindings) = resource.get("deploymentBindings") {
                for binding in bindings.as_array().ok_or("deploymentBindings must be an array")? {
                    let target = binding.get("targetRelativePath").and_then(serde_json::Value::as_str).ok_or("deployment binding target is missing")?;
                    let source = binding.get("packagePath").and_then(serde_json::Value::as_str).ok_or("deployment binding source is missing")?;
                    if target.rsplit_once('.').is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("dll") || extension.eq_ignore_ascii_case("exe")) {
                        paths.insert(source.into());
                    }
                }
            }
        }
    }
    for file in &mut manifest.files {
        if paths.contains(&file.path) { file.executable = true; }
    }
    Ok(paths)
}

fn file_identity(path: &Path) -> Result<(u64, String)> {
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
    }
    Ok((size, format!("sha256:{:x}", hash.finalize())))
}

fn copy_resources(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let output = destination.join(entry.file_name());
        if kind.is_dir() {
            fs::create_dir(&output)?;
            copy_resources(&entry.path(), &output)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), output)?;
        } else {
            return Err("game resources must be regular files or directories, never symlinks".into());
        }
    }
    Ok(())
}

fn files(root: &Path, directory: &Path, out: &mut Vec<ModuleFile>, executables: &BTreeSet<String>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files(root, &path, out, executables)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root)?.to_str().ok_or("non-UTF8 resource path")?.replace('\\', "/");
            let executable = executables.contains(&relative);
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }))?;
            }
            let (size, sha256) = file_identity(&path)?;
            out.push(ModuleFile { executable, path: relative, size, sha256 });
        } else {
            return Err("game payload must contain only regular files".into());
        }
    }
    Ok(())
}

fn build(directory: &Path, target: Option<&str>, patch_archive: Option<&Path>) -> Result<(Project, tempfile::TempDir)> {
    let directory = directory.canonicalize()?;
    let manifest_path = directory.join("Cargo.toml");
    let mut project: Project = read_json(&directory.join("module-project.json"))?;
    if let Some(target) = target {
        project.manifest.target = target.into();
        project.policy.target = target.into();
    } else if project.manifest.target == "host" {
        project.manifest.target = target_lexicon::HOST.to_string();
        project.policy.target = project.manifest.target.clone();
    }
    let triple = project.manifest.target.parse::<target_lexicon::Triple>().map_err(|error| error.to_string())?;
    if triple.to_string() != project.manifest.target || triple.operating_system == target_lexicon::OperatingSystem::Unknown || triple.architecture == target_lexicon::Architecture::Unknown {
        return Err("a canonical complete target triple is required".into());
    }
    if project.worker_binary.is_empty() || !project.worker_binary.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)) {
        return Err("workerBinary must name the game's Cargo binary".into());
    }
    let component = project.manifest.components.iter_mut().find(|component| component.runtime == "worker").ok_or("game requires its worker component")?;
    let previous_entry = component.entry.clone();
    if previous_entry.contains('/') || previous_entry.contains('\\') || previous_entry == ".." { return Err("worker entry must be a simple filename".into()); }
    let windows = project.manifest.target.contains("windows");
    let stem = previous_entry.strip_suffix(".exe").unwrap_or(&previous_entry);
    let worker = format!("{stem}{}", if windows { ".exe" } else { "" });
    component.entry = worker.clone();
    for file in &mut project.manifest.files { if file.path == previous_entry { file.path = worker.clone(); } }
    let executables = executable_paths(&mut project.manifest, &worker)?;
    validate_game_manifest(&project.manifest)?;
    project.policy.validate(&project.manifest)?;
    let metadata = Command::new("cargo").args(["metadata", "--locked", "--no-deps", "--format-version", "1", "--manifest-path"]).arg(&manifest_path).output()?;
    if !metadata.status.success() { return Err(String::from_utf8_lossy(&metadata.stderr).into_owned().into()); }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    let target_directory = PathBuf::from(metadata["target_directory"].as_str().ok_or("Cargo metadata lacks target_directory")?);
    let status = Command::new("cargo").args(["build", "--locked", "--release", "--bin", &project.worker_binary, "--manifest-path"]).arg(&manifest_path).args(["--target", &project.manifest.target]).status()?;
    if !status.success() { return Err("game worker build failed; diagnostics are above".into()); }
    let staging = target_directory.join("game-payloads");
    fs::create_dir_all(&staging)?;
    let payload = tempfile::Builder::new().prefix("game-").tempdir_in(staging)?;
    let resources = directory.join("payload");
    if resources.exists() {
        let metadata = fs::symlink_metadata(&resources)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() { return Err("game resources must be a real directory".into()); }
        copy_resources(&resources, payload.path())?;
    }
    if let Some(archive) = patch_archive {
        let [binding] = project.manifest.contributes["bundledResources"].as_array()
            .map(Vec::as_slice).unwrap_or_default() else {
            return Err("patch override requires exactly one bundled resource".into());
        };
        let path = binding["archivePath"].as_str().ok_or("project has no declared patch archive")?;
        // validate_game_manifest already checked that this path is portable and declared.
        let destination = payload.path().join(path);
        fs::create_dir_all(destination.parent().ok_or("patch archive parent is absent")?)?;
        fs::copy(archive, destination)?;
    }
    let executable = format!("{}{}", project.worker_binary, if windows { ".exe" } else { "" });
    fs::copy(target_directory.join(&project.manifest.target).join("release").join(executable), payload.path().join(&worker))?;
    project.manifest.files.clear();
    files(payload.path(), payload.path(), &mut project.manifest.files, &executables)?;
    project.manifest.files.sort_by(|left, right| left.path.cmp(&right.path));
    project.policy.validate(&project.manifest)?;
    validate_game_manifest(&project.manifest)?;
    module_signing_digest(payload.path(), &project.manifest)?;
    mintcat_runtime_release::validate_component_payloads(payload.path(), &project.manifest)?;
    Ok((project, payload))
}

pub fn check(directory: &Path, target: Option<&str>) -> Result<()> {
    let (project, _payload) = build(directory, target, None)?;
    println!("{}", serde_json::to_string_pretty(&json!({"game": project.manifest, "policy": project.policy, "status": "validated-build-and-payload"}))?);
    Ok(())
}

pub fn pack(directory: &Path, target: Option<&str>, key: &Path, output: &Path, patch_archive: Option<&Path>) -> Result<()> {
    let (project, payload) = build(directory, target, patch_archive)?;
    mintcat_runtime_artifacts::validate_production_origin(&project.manifest)?;
    let key = read_signing_key(key)?;
    let record = mintcat_runtime_release::pack(payload.path(), project.manifest, &key_id(&key.verifying_key()), &key, &project.policy, output)?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}
