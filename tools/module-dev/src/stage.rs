use std::{collections::{BTreeMap, BTreeSet}, fs, io::Write, path::{Component, Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};
use mintcat_runtime_release::{read_json, verify_package, LocalRepository, RepositoryBundle, RepositoryVerifier, VerificationPolicy, sha256, validate_native_executable, validate_native_payload};
use serde::{Deserialize, Serialize};
use serde_json::json;
type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;
const TARGETS: &[&str] = &["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];
#[derive(Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct Input {
    schema_version: u64, target: String, variant: String,
    root: PathBuf, policy: PathBuf, bundle: PathBuf, packages: Vec<PathBuf>,
    scopes: BTreeMap<String, ScopePin>, modules: BTreeSet<String>,
    binaries: BTreeMap<String, Binary>, output: PathBuf,
    #[serde(default)] fixed_webview2: Option<PathBuf>,
    /// Portable only: an explicit relative directory beside the executables, never a legacy database.
    #[serde(default)] portable_data_dir: Option<PathBuf>,
    #[serde(default)] sources: BTreeMap<String, mintcat_source_api::SourceWorkerConfig>,
}
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct ScopePin { release_id: String, sequence: u64, channel: String }
#[derive(Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct Binary { path: PathBuf, sha256: String }
#[derive(Deserialize, Serialize, PartialEq)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct FilePin { sha256: Option<String>, mode: u32 }
#[derive(Deserialize, Serialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct StageManifest {
    schema_version: u64, target: String, variant: String,
    scopes: BTreeMap<String, ScopePin>, modules: BTreeSet<String>,
    binaries: BTreeMap<String, String>, files: BTreeMap<String, FilePin>,
    data_directory: DataDirectory,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct DataDirectory { mode: String, #[serde(skip_serializing_if="Option::is_none")] path: Option<PathBuf> }
#[derive(Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
struct StagedRuntimeConfiguration {
    schema_version: u64, trust_root: PathBuf, verification_policy: PathBuf,
    repository_path: PathBuf, scopes: BTreeMap<String, ScopePin>,
    sources: BTreeMap<String, mintcat_source_api::SourceWorkerConfig>,
}
fn resolve(base: &Path, path: &Path) -> PathBuf { if path.is_absolute() { path.into() } else { base.join(path) } }
fn variant(target: &str, variant: &str) -> Result<()> {
    if !TARGETS.contains(&target) || !["standard", "portable", "fixed-webview2", "cli"].contains(&variant) { return Err("unsupported complete release target or variant".into()); }
    if matches!(variant, "portable" | "fixed-webview2") && target != "x86_64-pc-windows-msvc" { return Err("portable and fixed WebView2 require the exact Windows MSVC target".into()); }
    Ok(())
}
fn expected_binaries(variant: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::from(["mintcat".into(), "mintcatd".into(), "mintcat-module".into()]);
    if variant != "cli" { names.insert("mintcat-desktop-vnext".into()); }
    names
}
fn data_directory(variant: &str, path: Option<PathBuf>) -> Result<DataDirectory> {
    if variant == "portable" {
        let path = path.ok_or("portable release must declare portableDataDir")?;
        if path.as_os_str().is_empty() || path.components().any(|part| !matches!(part, Component::Normal(_))) { return Err("portableDataDir must be a nonempty relative child directory without traversal".into()); }
        let reserved = ["baseline", "trust", "webview2", "native", "compiler", "bundles", "runtime.json", "scope-locks.json", "release-manifest.json", "mintcat.exe", "mintcatd.exe", "mintcat-module.exe", "mintcat-desktop-vnext.exe"];
        if reserved.iter().any(|name| path.starts_with(name)) { return Err("portable data directory must not overlap release resources".into()); }
        Ok(DataDirectory { mode: "portable".into(), path: Some(path) })
    } else {
        if path.is_some() { return Err("portableDataDir is allowed only for portable releases".into()); }
        Ok(DataDirectory { mode: "explicit".into(), path: None })
    }
}
fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?; let kind = entry.file_type()?; let output = destination.join(entry.file_name());
        if kind.is_dir() { copy_tree(&entry.path(), &output)?; }
        else if kind.is_file() { fs::copy(entry.path(), output)?; }
        else { return Err("release resources must be regular files or directories, never symlinks".into()); }
    }
    fs::set_permissions(destination, fs::metadata(source)?.permissions())?;
    Ok(())
}
fn file_mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; metadata.permissions().mode() & 0o7777 }
    #[cfg(not(unix))] { if metadata.permissions().readonly() { 0o444 } else { 0o644 } }
}
fn inventory<const PUBLIC: bool>(base: &Path, directory: &Path, files: &mut BTreeMap<String, FilePin>, exclude_manifest: bool) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?; let path = entry.path(); let metadata = fs::symlink_metadata(&path)?;
        let name = path.strip_prefix(base)?.to_str().ok_or("release paths must be UTF-8")?.replace('\\', "/");
        if exclude_manifest && name == "release-manifest.json" { continue; }
        if !metadata.is_dir() && (!metadata.is_file() || metadata.file_type().is_symlink()) { return Err("immutable stage contains a symlink or nonregular file".into()); }
        let mode = file_mode(&metadata);
        #[cfg(unix)]
        let mode = if PUBLIC {
            use std::os::unix::fs::PermissionsExt;
            let mode = if metadata.is_dir() || mode & 0o111 != 0 { 0o755 } else { 0o644 };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
            mode
        } else { mode };
        if metadata.is_dir() {
            files.insert(name, FilePin { sha256: None, mode });
            inventory::<PUBLIC>(base, &path, files, exclude_manifest)?;
        }
        else { files.insert(name, FilePin { sha256: Some(sha256(&fs::read(path)?)), mode }); }
    }
    Ok(())
}
fn check_binary(path: &Path, target: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() { return Err("release binary must be a regular file".into()); }
    #[cfg(unix)] if !target.contains("windows") && file_mode(&metadata) & 0o111 == 0 { return Err("release native binary lacks executable permissions".into()); }
    validate_native_executable(path, target)?;
    Ok(())
}
fn check_webview_runtime(directory: &Path, target: &str) -> Result<()> {
    if !directory.join("msedgewebview2.exe").is_file() { return Err("fixed WebView2 runtime lacks msedgewebview2.exe".into()); }
    check_webview_native_files(directory, target)
}
fn check_webview_native_files(directory: &Path, target: &str) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?; let kind = entry.file_type()?; let path = entry.path();
        if kind.is_dir() { check_webview_native_files(&path, target)?; }
        else if kind.is_file() {
            match path.extension().and_then(|extension| extension.to_str()) {
                Some(extension) if extension.eq_ignore_ascii_case("exe") => check_binary(&path, target)?,
                Some(extension) if extension.eq_ignore_ascii_case("dll") => validate_native_payload(&path, target)?,
                _ => {},
            }
        } else { return Err("fixed WebView2 runtime must not contain symlinks or nonregular files".into()); }
    }
    Ok(())
}
fn check_closure(verified: &[mintcat_runtime_release::VerifiedScopeRelease], records: &BTreeMap<String, mintcat_runtime_release::PackageRecord>, scopes: &BTreeMap<String, ScopePin>, modules: &BTreeSet<String>) -> Result<()> {
    let mut actual_scopes = BTreeMap::new(); let mut actual_modules = BTreeSet::new(); let mut needed = BTreeSet::new(); let mut kinds = BTreeSet::new();
    for scope in verified {
        let release = scope.release();
        if actual_scopes.insert(release.scope_id.clone(), ScopePin { release_id: release.release_id.clone(), sequence: release.sequence, channel: release.channel.clone() }).is_some() { return Err("ambiguous duplicate baseline scope".into()); }
        for (digest, record) in scope.package_records() {
            if records.get(digest) != Some(record) { return Err("offline package differs from signed scope lock".into()); }
            needed.insert(digest.clone()); actual_modules.insert(record.manifest.id.clone());
            for component in &record.manifest.components { kinds.insert(component.kind.clone()); }
        }
    }
    if &actual_scopes != scopes || !scopes.contains_key("platform") { return Err("exact platform/game scope locks differ from the release plan".into()); }
    if &actual_modules != modules || modules.is_empty() || needed != records.keys().cloned().collect() { return Err("complete module/package closure differs from the release plan".into()); }
    if !["service", "resource", "ui"].iter().all(|kind| kinds.contains(*kind)) { return Err("baseline must contain signed worker, resource and UI components".into()); }
    Ok(())
}
pub fn stage(manifest_path: &Path) -> Result<()> {
    let input: Input = read_json(manifest_path)?;
    if input.schema_version != 1 { return Err("unsupported release schema".into()); }
    variant(&input.target, &input.variant)?;
    let data_directory = data_directory(&input.variant, input.portable_data_dir)?;
    if (input.variant == "fixed-webview2") != input.fixed_webview2.is_some() { return Err("fixed WebView2 requires an authenticated runtime tree; other variants must not supply one".into()); }
    if input.binaries.keys().cloned().collect::<BTreeSet<_>>() != expected_binaries(&input.variant) { return Err("release plan must pin the exact mintcat, mintcatd, mintcat-module and (for GUI) desktop binaries".into()); }
    let base = manifest_path.parent().unwrap_or(Path::new("."));
    let output = resolve(base, &input.output);
    if output.exists() { return Err("release stage already exists; choose a fresh immutable directory".into()); }
    let root_path = resolve(base, &input.root);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let policy: VerificationPolicy = read_json(&resolve(base, &input.policy))?;
    if policy.target != input.target || policy.host_api != "1" { return Err("release target/host API differs from verification policy".into()); }
    let bundle: RepositoryBundle = read_json(&resolve(base, &input.bundle))?;
    let packages = input.packages.iter().map(|p| resolve(base, p)).collect::<Vec<_>>();
    let parent = output.parent().unwrap_or(Path::new(".")); fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".release-stage-{}", uuid::Uuid::new_v4())); fs::create_dir(&temporary)?;
    let attempt: Result<()> = (|| {
        let verifier = RepositoryVerifier::from_pinned_file(temporary.join("verification-state.json"), &root_path)?;
        let verified = verifier.verify(&bundle, &policy, now)?; let root = verifier.trusted_root(now)?;
        let mut records = BTreeMap::new();
        for package in &packages { let record = verify_package(package, &root, &policy)?; if records.insert(record.digest.clone(), record).is_some() { return Err("duplicate supplied package".into()); } }
        check_closure(&verified, &records, &input.scopes, &input.modules)?;
        check_sources(&records, &input.sources)?;
        let extension = if input.target.contains("windows") { ".exe" } else { "" };
        for (name, binary) in &input.binaries {
            let source = resolve(base, &binary.path); check_binary(&source, &input.target)?;
            if sha256(&fs::read(&source)?) != binary.sha256 { return Err("native binary hash differs from the release plan".into()); }
            let destination = temporary.join(format!("{name}{extension}")); fs::copy(&source, &destination)?;
            if sha256(&fs::read(&destination)?) != binary.sha256 { return Err("native binary changed while staging".into()); }
        }
        LocalRepository::open(&temporary.join("baseline"))?.publish_bundle(&bundle, &packages, &root, &policy, now)?;
        fs::create_dir(temporary.join("trust"))?;
        let mut options = fs::OpenOptions::new(); options.create_new(true).write(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o644); }
        let mut pinned = options.open(temporary.join("trust/root.json"))?; pinned.write_all(&serde_json::to_vec(root.envelope())?)?; pinned.sync_all()?;
        fs::write(temporary.join("trust/policy.json"), serde_json::to_vec_pretty(&policy)?)?;
        fs::write(temporary.join("scope-locks.json"), serde_json::to_vec_pretty(&input.scopes)?)?;
        fs::write(temporary.join("runtime.json"), serde_json::to_vec_pretty(&json!({"schemaVersion":1,"trustRoot":"trust/root.json","verificationPolicy":"trust/policy.json","repositoryPath":"baseline","scopes":input.scopes,"sources":input.sources}))?)?;
        if let Some(runtime) = &input.fixed_webview2 {
            let runtime = fs::canonicalize(resolve(base, runtime))?;
            let authenticated = packages.iter().any(|package| fs::canonicalize(package.join("payload")).ok().as_ref() == Some(&runtime) && read_json::<mintcat_sdk::ModuleManifest>(&package.join("module.json")).map(|manifest| records.values().any(|record| record.manifest == manifest && manifest.components.iter().any(|component| component.kind == "resource" && component.runtime == "data"))).unwrap_or(false));
            if !authenticated { return Err("fixed WebView2 must be the payload of a verified signed baseline resource package".into()); }
            check_webview_runtime(&runtime, &input.target)?; copy_tree(&runtime, &temporary.join("webview2"))?;
        }
        fs::remove_file(temporary.join("verification-state.json"))?;
        fs::remove_file(temporary.join("verification-state.release-lock"))?;
        // Repository producer state uses private permissions. A shipped baseline
        // contains only public signed distribution data and must remain readable
        // when the installer owns it and runtime state belongs to another user.
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
        }
        let mut files = BTreeMap::new(); inventory::<true>(&temporary, &temporary, &mut files, true)?;
        let manifest = StageManifest { schema_version: 1, target: input.target.clone(), variant: input.variant.clone(), scopes: input.scopes, modules: input.modules, binaries: input.binaries.into_iter().map(|(name, binary)| (name, binary.sha256)).collect(), files, data_directory };
        fs::write(temporary.join("release-manifest.json"), serde_json::to_vec_pretty(&manifest)?)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(temporary.join("release-manifest.json"), fs::Permissions::from_mode(0o644))?;
        }
        fs::rename(&temporary, &output)?; Ok(())
    })();
    if temporary.exists() { let _ = fs::remove_dir_all(&temporary); }
    attempt?;
    println!("Verified immutable {} stage at {} for {}", input.variant, output.display(), input.target);
    println!("Stage manifest pin: {}", sha256(&fs::read(output.join("release-manifest.json"))?));
    Ok(())
}
/// Reauthenticate an existing stage without writing any bytes into it.
pub fn verify_stage(directory: &Path, target: &str, requested_variant: &str, manifest_sha256: &str) -> Result<()> {
    variant(target, requested_variant)?;
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() { return Err("stage must be an existing real directory".into()); }
    let manifest_metadata = fs::symlink_metadata(directory.join("release-manifest.json"))?;
    if !manifest_metadata.is_file() || manifest_metadata.file_type().is_symlink() { return Err("stage manifest must be a regular file".into()); }
    let bytes = fs::read(directory.join("release-manifest.json"))?;
    if sha256(&bytes) != manifest_sha256 { return Err("immutable stage manifest pin differs from the assembly plan".into()); }
    let manifest: StageManifest = serde_json::from_slice(&bytes)?;
    if manifest.schema_version != 1 || manifest.target != target || manifest.variant != requested_variant { return Err("stage schema/target/variant differs from the assembly plan".into()); }
    data_directory(requested_variant, manifest.data_directory.path.clone()).and_then(|expected| if expected.mode == manifest.data_directory.mode { Ok(expected) } else { Err("invalid declared data directory mode".into()) })?;
    if manifest.binaries.keys().cloned().collect::<BTreeSet<_>>() != expected_binaries(requested_variant) { return Err("incomplete stage native binaries".into()); }
    let mut files = BTreeMap::new(); inventory::<false>(directory, directory, &mut files, true)?;
    if files != manifest.files { return Err("immutable stage bytes or permissions changed after verification".into()); }
    let extension = if target.contains("windows") { ".exe" } else { "" };
    for (name, digest) in &manifest.binaries { let path = directory.join(format!("{name}{extension}")); check_binary(&path, target)?; if sha256(&fs::read(path)?) != *digest { return Err("stage binary hash differs from compiled release pin".into()); } }
    let policy: VerificationPolicy = read_json(&directory.join("trust/policy.json"))?;
    if policy.target != target || policy.host_api != "1" { return Err("stage verification policy target/host API mismatch".into()); }
    let temporary = std::env::temp_dir().join(format!("mintcat-stage-verify-{}", uuid::Uuid::new_v4())); fs::create_dir(&temporary)?;
    let result: Result<()> = (|| {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let verifier = RepositoryVerifier::from_pinned_file(temporary.join("state.json"), &directory.join("trust/root.json"))?;
        let bundle: RepositoryBundle = read_json(&directory.join("baseline/current.json"))?;
        let verified = verifier.verify(&bundle, &policy, now)?; let root = verifier.trusted_root(now)?;
        let mut records = BTreeMap::new();
        for entry in fs::read_dir(directory.join("baseline/artifacts/sha256"))? { let entry = entry?; let record = verify_package(&entry.path(), &root, &policy)?; if entry.file_name().to_str() != Some(&record.digest[7..]) || records.insert(record.digest.clone(), record).is_some() { return Err("stage artifact identity collision".into()); } }
        check_closure(&verified, &records, &manifest.scopes, &manifest.modules)?;
        let configuration: StagedRuntimeConfiguration = read_json(&directory.join("runtime.json"))?;
        let scope_locks: BTreeMap<String, ScopePin> = read_json(&directory.join("scope-locks.json"))?;
        if configuration.schema_version != 1 || configuration.trust_root != Path::new("trust/root.json") || configuration.verification_policy != Path::new("trust/policy.json") || configuration.repository_path != Path::new("baseline") ||
            configuration.scopes != manifest.scopes || scope_locks != manifest.scopes {
            return Err("stage runtime configuration differs from the exact relocatable baseline".into());
        }
        check_sources(&records, &configuration.sources)?;
        if requested_variant == "fixed-webview2" {
            check_webview_runtime(&directory.join("webview2"), target)?;
            let mut webview_files = BTreeMap::new(); inventory::<false>(&directory.join("webview2"), &directory.join("webview2"), &mut webview_files, false)?;
            let mut authenticated = false;
            for (digest, record) in &records {
                if record.manifest.components.iter().any(|component| component.kind == "resource" && component.runtime == "data") {
                    let payload = directory.join("baseline/artifacts/sha256").join(&digest[7..]).join("payload");
                    let mut package_files = BTreeMap::new(); inventory::<false>(&payload, &payload, &mut package_files, false)?;
                    if package_files == webview_files { authenticated = true; }
                }
            }
            if !authenticated { return Err("fixed WebView2 bytes do not match an authenticated resource payload".into()); }
        }
        Ok(())
    })();
    let cleanup = fs::remove_dir_all(&temporary);
    result?; cleanup?;
    println!("Verified immutable {} stage for {}: {}", requested_variant, target, directory.display());
    Ok(())
}

fn check_sources(records: &BTreeMap<String, mintcat_runtime_release::PackageRecord>, sources: &BTreeMap<String, mintcat_source_api::SourceWorkerConfig>) -> Result<()> {
    let expected = records.values().filter_map(|record| record.manifest.id.strip_prefix("source-").map(str::to_owned)).collect::<BTreeSet<_>>();
    if sources.keys().cloned().collect::<BTreeSet<_>>() != expected { return Err("runtime sources must exactly configure every shipped signed source provider".into()); }
    for config in sources.values() {
        config.download_origins()?; config.navigation_origins()?;
    }
    Ok(())
}
