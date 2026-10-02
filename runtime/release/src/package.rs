use std::{collections::{BTreeMap, BTreeSet}, fs::{self, File}, io::{Read, Write}, path::{Component, Path}};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signer, SigningKey};
use mintcat_runtime_artifacts::module_signing_digest;
use mintcat_sdk::{ModuleManifest, ModuleSignature, PermissionRequest, SignatureAlgorithm};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::{canonical, read_json, require, sha256, Result, TrustedRoot};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationPolicy {
    pub target: String,
    pub host_api: String,
    pub max_data_schema: u64,
    pub permissions: Vec<PermissionRequest>,
    pub contracts: BTreeMap<String, BTreeSet<u64>>,
}
impl VerificationPolicy {
    pub fn validate(&self, manifest: &ModuleManifest) -> Result<()> {
        require(manifest.schema_version == 1, "unsupported package schema")?;
        require(manifest.target == self.target, "package target mismatch")?;
        let target: target_lexicon::Triple = manifest.target.parse().map_err(|e| crate::ReleaseError::Rejected(format!("invalid target triple: {e}")))?;
        require(target.architecture != target_lexicon::Architecture::Unknown && target.operating_system != target_lexicon::OperatingSystem::Unknown, "target must identify an architecture and operating system")?;
        require(target.to_string() == manifest.target, "target must be a canonical full triple")?;
        require(manifest.host_api == self.host_api && !self.host_api.is_empty(), "host API mismatch")?;
        require(manifest.contributes.is_object(), "contributes must be an object")?;
        for value in [&manifest.id, &manifest.publisher] { require(!value.trim().is_empty() && value.len() <= 200, "invalid module identity")?; }
        require(semver::Version::parse(&manifest.version).is_ok(), "module version must be semver")?;
        require(manifest.state.schema_version > 0 && manifest.state.schema_version <= self.max_data_schema, "unsupported data schema")?;
        require(version_matches(&manifest.state.readable_range, manifest.state.schema_version)? && version_matches(&manifest.state.writable_range, manifest.state.schema_version)?, "declared state schema is not readable/writable")?;
        require(matches!(manifest.activation.trigger.as_str(), "on-demand" | "startup"), "unsupported activation trigger")?;
        require(matches!(manifest.activation.drain_policy.as_str(), "finish-requests" | "drain" | "none"), "unsupported drain policy")?;
        let mut paths = BTreeSet::new();
        for file in &manifest.files {
            checked_path(&file.path)?;
            require(paths.insert(file.path.as_str()), "duplicate file path")?;
            require(file.sha256.starts_with("sha256:") && file.sha256.len() == 71 && file.sha256[7..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "invalid payload digest")?;
        }
        let mut entries = BTreeSet::new();
        for component in &manifest.components {
            require(entries.insert((&component.kind, &component.entry)), "duplicate component")?;
            let file = manifest.files.iter().find(|f| f.path == component.entry).ok_or_else(|| crate::ReleaseError::Rejected("component entry not in file list".into()))?;
            require(matches!((component.kind.as_str(), component.runtime.as_str()), ("service", "worker" | "native-library" | "mintcat.proxy-service/1") | ("ui", "webview") | ("resource", "data") | ("native", "payload")), "unsupported component kind/runtime")?;
            require(matches!(component.state_model.as_str(), "stateless" | "single-writer" | "daemon-owned"), "unsupported component state model")?;
            if matches!(component.runtime.as_str(), "worker" | "mintcat.proxy-service/1") { require(file.executable, "service executable entry must be executable")?; }
            if matches!(component.kind.as_str(), "ui" | "resource") { require(!file.executable, "UI/resource entry must not be executable")?; }
        }
        let mut provided = BTreeSet::new();
        for contract in &manifest.provides {
            require(!contract.contract_id.trim().is_empty() && contract.contract_id.len() <= 200 && contract.version > 0 && provided.insert((&contract.contract_id, contract.version)), "invalid or duplicate provided contract")?;
            require(self.contracts.get(&contract.contract_id).is_some_and(|v| v.contains(&contract.version)), "provided contract not approved by host policy")?;
            require(contract.cardinality.as_deref().map_or(true, |v| matches!(v, "single" | "singleton" | "multi")), "invalid contract cardinality")?;
        }
        let mut required = BTreeSet::new();
        for contract in &manifest.requires {
            require(!contract.contract_id.trim().is_empty() && contract.contract_id.len() <= 200, "invalid required contract identity")?;
            require(required.insert((&contract.contract_id, &contract.binding_scope)), "duplicate required contract")?;
            if contract.binding_scope.as_deref() == Some("host") {
                // Policy approval alone does not turn a module contract into a host service.
                // This immutable built-in matches the supervisor's host service registry.
                let compatible = version_matches(&contract.version_range, 1)?;
                let available = contract.contract_id == "source.network" && compatible
                    && self.contracts.get(&contract.contract_id).is_some_and(|versions| versions.contains(&1));
                require(contract.optional || available, "required host contract unavailable or incompatible")?;
                continue;
            }
            let approved = self.contracts.get(&contract.contract_id).ok_or_else(|| crate::ReleaseError::Rejected("required contract not approved by host policy".into()))?;
            let mut supported = false;
            for version in approved { if version_matches(&contract.version_range, *version)? { supported = true; break; } }
            require(supported, "required contract has no host-approved version")?;
        }
        let mut permissions = BTreeSet::new();
        for permission in &manifest.permissions {
            require(!permission.capability.trim().is_empty() && permission.capability.len() <= 200 && !permission.scope.trim().is_empty(), "invalid permission request")?;
            require(permissions.insert((&permission.capability, &permission.scope)), "duplicate permission")?;
            require(self.permissions.contains(permission), format!("unapproved permission {}:{}", permission.capability, permission.scope))?;
        }
        Ok(())
    }
}

pub fn version_matches(range: &str, version: u64) -> Result<bool> {
    // Integer contracts/schemas use semver major versions. Bare integers are exact.
    if let Ok(exact) = range.parse::<u64>() { return Ok(exact == version); }
    let normalized = if range.contains(',') || !range.chars().any(char::is_whitespace) {
        std::borrow::Cow::Borrowed(range)
    } else {
        let mut normalized = String::with_capacity(range.len() + 4);
        for (index, part) in range.split_whitespace().enumerate() {
            if index != 0 { normalized.push_str(", "); }
            normalized.push_str(part);
        }
        std::borrow::Cow::Owned(normalized)
    };
    let req = semver::VersionReq::parse(&normalized)
        .map_err(|e| crate::ReleaseError::Rejected(format!("invalid version range {range}: {e}")))?;
    Ok(req.matches(&semver::Version::new(version, 0, 0)))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageRecord {
    pub manifest: ModuleManifest,
    pub signature: ModuleSignature,
    pub digest: String,
    pub length: u64,
    pub record_digest: String,
}

pub fn read_signing_key(path: &Path) -> Result<SigningKey> {
    let metadata = fs::symlink_metadata(path)?;
    require(metadata.is_file() && !metadata.file_type().is_symlink(), "private key must be a regular file")?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        require(metadata.permissions().mode() & 0o077 == 0, "private key permissions must deny group/other access (0600)")?;
    }
    let text = fs::read_to_string(path)?;
    let seed = BASE64.decode(text.trim()).map_err(|_| crate::ReleaseError::Rejected("private key must be a base64 Ed25519 32-byte seed".into()))?;
    let seed: [u8; 32] = seed.try_into().map_err(|_| crate::ReleaseError::Rejected("Ed25519 seed must contain 32 bytes".into()))?;
    Ok(SigningKey::from_bytes(&seed))
}

pub fn pack(source: &Path, mut manifest: ModuleManifest, key_id: &str, key: &SigningKey, policy: &VerificationPolicy, output: &Path) -> Result<PackageRecord> {
    let source_metadata = fs::symlink_metadata(source)?;
    require(source_metadata.is_dir() && !source_metadata.file_type().is_symlink(), "package source must be a real directory")?;
    policy.validate(&manifest)?;
    require(!key_id.trim().is_empty() && key_id.len() <= 200, "invalid package signing key identity")?;
    manifest.files.sort_by(|a, b| a.path.cmp(&b.path));
    let digest = module_signing_digest(source, &manifest)?;
    validate_component_payloads(source, &manifest)?;
    let signature = ModuleSignature { schema_version: 1, publisher: manifest.publisher.clone(), key_id: key_id.into(), algorithm: SignatureAlgorithm::Ed25519, signature: BASE64.encode(key.sign(&digest).to_bytes()) };
    require(!output.exists(), "package output already exists")?;
    let parent = crate::parent_directory(output);
    fs::create_dir_all(parent)?;
    let stage = parent.join(format!(".pack-{}", uuid::Uuid::new_v4()));
    let result: Result<PackageRecord> = (|| {
        fs::create_dir(&stage)?;
        copy_payload(source, &stage.join("payload"), &manifest)?;
        fs::write(stage.join("module.json"), canonical(&manifest)?)?;
        fs::write(stage.join("signature.json"), canonical(&signature)?)?;
        let copied = module_signing_digest(&stage.join("payload"), &manifest)?;
        require(copied == digest, "payload changed during packaging")?;
        sync_package(&stage)?;
        let record = record(manifest, signature, digest)?;
        crate::commit_rename(&stage, output)?;
        Ok(record)
    })();
    if stage.exists() { let _ = fs::remove_dir_all(&stage); }
    result
}

pub fn verify_package(path: &Path, root: &TrustedRoot, policy: &VerificationPolicy) -> Result<PackageRecord> {
    let package_metadata = fs::symlink_metadata(path)?;
    require(package_metadata.is_dir() && !package_metadata.file_type().is_symlink(), "package must be a real directory")?;
    let mut actual = fs::read_dir(path)?.map(|e| e.map(|e| e.file_name())).collect::<std::io::Result<Vec<_>>>()?;
    actual.sort();
    require(actual == ["module.json", "payload", "signature.json"].map(std::ffi::OsString::from), "unexpected package root entries")?;
    let payload = fs::symlink_metadata(path.join("payload"))?;
    require(payload.is_dir() && !payload.file_type().is_symlink(), "payload must be a real directory")?;
    let manifest: ModuleManifest = read_json(&path.join("module.json"))?;
    let signature: ModuleSignature = read_json(&path.join("signature.json"))?;
    require(fs::read(path.join("module.json"))? == canonical(&manifest)? && fs::read(path.join("signature.json"))? == canonical(&signature)?, "package metadata must use canonical exact bytes")?;
    policy.validate(&manifest)?;
    let digest = module_signing_digest(&path.join("payload"), &manifest)?;
    validate_component_payloads(&path.join("payload"), &manifest)?;
    root.verify_package_signature(&manifest, &signature, &digest)?;
    for file in &manifest.files {
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let executable = fs::metadata(path.join("payload").join(&file.path))?.permissions().mode() & 0o111 != 0;
            require(executable == file.executable, "payload executable permission differs from signed manifest")?;
        }
    }
    record(manifest, signature, digest)
}

fn record(manifest: ModuleManifest, signature: ModuleSignature, digest: [u8; 32]) -> Result<PackageRecord> {
    let (length, record_digest) = package_measurements(&manifest, &signature)?;
    let digest = format!("sha256:{:x}", sha2::digest::Output::<Sha256>::from(digest));
    Ok(PackageRecord { manifest, signature, digest, length, record_digest })
}

fn package_measurements(manifest: &ModuleManifest, signature: &ModuleSignature) -> Result<(u64, String)> {
    let manifest_bytes = canonical(manifest)?;
    let signature_bytes = canonical(signature)?;
    let mut length = (manifest_bytes.len() as u64).checked_add(signature_bytes.len() as u64).ok_or_else(|| crate::ReleaseError::Rejected("package length overflow".into()))?;
    for file in &manifest.files { length = length.checked_add(file.size).ok_or_else(|| crate::ReleaseError::Rejected("package length overflow".into()))?; }
    let record_digest = sha256(&canonical(&(manifest, signature))?);
    Ok((length, record_digest))
}

pub(crate) fn checked_path(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    require(!value.is_empty() && !value.contains('\\') && path.components().all(|c| matches!(c, Component::Normal(_))), "unsafe relative path")?;
    require(path.to_str() == Some(value) && !value.contains("//") && !value.ends_with('/'), "noncanonical relative path")?;
    Ok(path)
}

pub(crate) fn copy_payload(source: &Path, output: &Path, manifest: &ModuleManifest) -> Result<()> {
    fs::create_dir_all(output)?;
    let mut buffer = [0u8; 65536];
    for item in &manifest.files {
        let relative = checked_path(&item.path)?;
        let destination = output.join(relative);
        fs::create_dir_all(destination.parent().expect("payload parent"))?;
        let mut input = File::open(source.join(relative))?;
        let mut target = fs::OpenOptions::new().create_new(true).write(true).open(&destination)?;
        let mut hash = Sha256::new();
        let mut length = 0u64;
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 { break; }
            length += n as u64;
            require(length <= item.size, "payload grew during copying")?;
            hash.update(&buffer[..n]);
            target.write_all(&buffer[..n])?;
        }
        require(length == item.size && format!("sha256:{:x}", hash.finalize()) == item.sha256, "payload changed during copying")?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            target.set_permissions(fs::Permissions::from_mode(if item.executable { 0o755 } else { 0o644 }))?;
        }
        target.sync_all()?;
    }
    Ok(())
}
pub(crate) fn sync_package(path: &Path) -> Result<()> {
    // Payload handles were already synced by copy_payload before they were closed.
    for name in ["module.json", "signature.json"] {
        fs::OpenOptions::new().read(true).write(true).open(path.join(name))?.sync_all()?;
    }
    #[cfg(unix)] {
        sync_payload_directories(&path.join("payload"))?;
        File::open(path)?.sync_all()?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_payload_directories(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() { sync_payload_directories(&entry.path())?; }
    }
    File::open(path)?.sync_all()?;
    Ok(())
}
pub(crate) fn validate_package_record(package: &PackageRecord, root: &TrustedRoot, policy: &VerificationPolicy) -> Result<()> {
    policy.validate(&package.manifest)?;
    let digest = mintcat_runtime_artifacts::module_manifest_signing_digest(&package.manifest)?;
    root.verify_package_signature(&package.manifest, &package.signature, &digest)?;
    let (length, record_digest) = package_measurements(&package.manifest, &package.signature)?;
    require(package.digest == format!("sha256:{:x}", sha2::digest::Output::<Sha256>::from(digest)) && package.length == length && package.record_digest == record_digest, "signed package digest/length/record mismatch")
}

/// Check component payload types after manifest policy and complete-file validation.
pub fn validate_component_payloads(source: &Path, manifest: &ModuleManifest) -> Result<()> {
    for component in &manifest.components {
        checked_path(&component.entry)?;
        if component.kind == "native" || matches!(component.runtime.as_str(), "worker" | "native-library" | "mintcat.proxy-service/1") {
            validate_native_entry(&source.join(&component.entry), &manifest.target, &component.runtime)?;
        }
    }
    let mut components = manifest.components.iter().filter(|c| c.kind == "ui");
    let Some(ui) = components.next() else { return Ok(()); };
    require(components.next().is_none(), "a UI package must have one ESM entry")?;
    require(manifest.files.iter().any(|f| f.path == "ui/contributions.json" && !f.executable), "UI descriptor absent from signed file list")?;
    let descriptor: serde_json::Value = read_json(&source.join("ui/contributions.json"))?;
    let descriptor = descriptor.as_object().ok_or_else(|| crate::ReleaseError::Rejected("UI descriptor must be an object".into()))?;
    let fields = ["schemaVersion", "runtimeVersion", "moduleId", "entry", "css", "pages", "navigation", "commands", "settings", "games"];
    require(descriptor.keys().all(|key| fields.contains(&key.as_str())), "unknown UI descriptor field")?;
    require(descriptor.get("schemaVersion").and_then(serde_json::Value::as_u64) == Some(1) && descriptor.get("runtimeVersion").and_then(serde_json::Value::as_u64) == Some(1), "unsupported UI ABI/schema")?;
    require(descriptor.get("moduleId").and_then(serde_json::Value::as_str) == Some(manifest.id.as_str()), "UI descriptor module identity mismatch")?;
    require(descriptor.get("entry").and_then(serde_json::Value::as_str) == Some(ui.entry.as_str()), "UI descriptor/component entry mismatch")?;
    require(ui.entry.starts_with("ui/") && (ui.entry.ends_with(".js") || ui.entry.ends_with(".mjs")), "UI entry must be signed ESM under ui/")?;
    for name in ["css", "pages", "navigation", "commands", "settings", "games"] {
        require(descriptor.get(name).and_then(serde_json::Value::as_array).map_or(false, |entries| entries.len() <= 200), "UI contribution arrays must be present and bounded")?;
    }
    for css in descriptor["css"].as_array().expect("validated list") {
        let css = css.as_str().ok_or_else(|| crate::ReleaseError::Rejected("UI CSS must be a path".into()))?;
        checked_path(css)?;
        require(css.starts_with("ui/") && css.ends_with(".css") && manifest.files.iter().any(|f| f.path == css && !f.executable), "UI CSS absent from signed file list")?;
    }
    // Page/navigation/command/settings semantics are validated by the SDK UI loader.
    // This boundary authenticates their ABI, identity and complete asset membership.
    Ok(())
}
/// Validates actual native image format, architecture and loadable image kind.
/// This is not a signature/trust check; stage tooling must also authenticate bytes.
pub fn validate_native_payload(path: &Path, target: &str) -> Result<()> {
    validate_native_entry(path, target, "payload")
}

/// Validates a runnable native sidecar, not a dynamic library with a matching architecture.
/// ELF position-independent executables require a nonzero entry point.
pub fn validate_native_executable(path: &Path, target: &str) -> Result<()> {
    validate_native_entry(path, target, "worker")
}

fn validate_native_entry(path: &Path, target: &str, runtime: &str) -> Result<()> {
    use object::Object;
    let cache = object::read::ReadCache::new(File::open(path)?);
    let binary = object::File::parse(&cache).map_err(|error| crate::ReleaseError::Rejected(format!("invalid native entry {}: {error}", path.display())))?;
    let target_text = target;
    let target: target_lexicon::Triple = target.parse().map_err(|error| crate::ReleaseError::Rejected(format!("invalid native target: {error}")))?;
    require(target.architecture != target_lexicon::Architecture::Unknown && target.operating_system != target_lexicon::OperatingSystem::Unknown && target.to_string() == target_text, "native target must be a canonical full triple")?;
    let format = match target.binary_format {
        target_lexicon::BinaryFormat::Elf => binary.format() == object::BinaryFormat::Elf,
        target_lexicon::BinaryFormat::Macho => binary.format() == object::BinaryFormat::MachO,
        target_lexicon::BinaryFormat::Coff => binary.format() == object::BinaryFormat::Pe,
        _ => false,
    };
    require(format, "native binary format does not match target OS")?;
    let architecture = match target.architecture {
        target_lexicon::Architecture::Aarch64(_) => binary.architecture() == object::Architecture::Aarch64,
        target_lexicon::Architecture::X86_64 | target_lexicon::Architecture::X86_64h => binary.architecture() == object::Architecture::X86_64,
        target_lexicon::Architecture::X86_32(_) => binary.architecture() == object::Architecture::I386,
        target_lexicon::Architecture::Arm(_) => binary.architecture() == object::Architecture::Arm,
        _ => false,
    };
    require(architecture, "native binary architecture does not match supported target")?;
    let endianness = target.endianness().map_err(|_| crate::ReleaseError::Rejected("unsupported native target byte order".into()))?;
    require(binary.is_little_endian() == (endianness == target_lexicon::Endianness::Little), "native binary byte order does not match target")?;
    let width = target.pointer_width().map_err(|_| crate::ReleaseError::Rejected("unsupported native pointer width".into()))?;
    require(binary.is_64() == (width == target_lexicon::PointerWidth::U64), "native binary word size does not match target ABI")?;
    if runtime == "native-library" {
        require(binary.kind() == object::ObjectKind::Dynamic, "native library entry must be a dynamic library")?;
    } else if runtime == "payload" {
        require(matches!(binary.kind(), object::ObjectKind::Executable | object::ObjectKind::Dynamic), "native payload must be an executable or dynamic library")?;
    } else {
        // ELF PIEs use ET_DYN; a nonzero entry point distinguishes executable PIEs.
        require(binary.kind() == object::ObjectKind::Executable || (binary.format() == object::BinaryFormat::Elf && binary.kind() == object::ObjectKind::Dynamic && binary.entry() != 0), "native worker/service entry must be executable")?;
    }
    Ok(())
}
