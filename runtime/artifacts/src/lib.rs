mod worker_grants;
pub use worker_grants::WorkerModuleGrant;

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimeLayout};
use mintcat_sdk::{
    ArtifactGrantAccess, ArtifactGrantDescriptor, ArtifactRef, ModuleManifest, ModuleSignature,
    SignatureAlgorithm,
};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

const PACKAGE_DOMAIN: &[u8] = b"mintcat-module-package-v1\0";
const MODULE_MANIFEST_FILE: &str = "module.json";
const MODULE_SIGNATURE_FILE: &str = "signature.json";
const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("runtime layout is not initialized")]
    UninitializedLayout,
    #[error("artifact I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("artifact metadata is invalid: {0}")]
    InvalidMetadata(#[from] serde_json::Error),
    #[error("module manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("module target {actual} does not match host target {expected}")]
    TargetMismatch { expected: String, actual: String },
    #[error("module host API {actual} does not match host API {expected}")]
    HostApiMismatch { expected: String, actual: String },
    #[error("module file {path} is invalid: {message}")]
    InvalidFile { path: String, message: String },
    #[error("module payload file list differs from the manifest")]
    FileListMismatch,
    #[error("module signature publisher does not match the manifest")]
    SignaturePublisherMismatch,
    #[error("module signature encoding is invalid")]
    InvalidSignatureEncoding,
    #[error("module signer {publisher}/{key_id} is not trusted for {environment:?}")]
    UntrustedSigner {
        publisher: String,
        key_id: String,
        environment: RuntimeEnvironment,
    },
    #[error("module signature verification failed")]
    SignatureVerification,
    #[error("artifact digest {0} is invalid")]
    InvalidDigest(String),
    #[error("artifact {0} was not found")]
    NotFound(String),
    #[error("artifact {0} is not verified")]
    UnverifiedArtifact(String),
}

#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    production: HashMap<(String, String), VerifyingKey>,
    development: HashMap<(String, String), VerifyingKey>,
    modules: Option<HashMap<[u8; 32], ScopedModuleAuthority>>,
}

#[derive(Debug, Clone)]
struct ScopedModuleAuthority { signature: ModuleSignature, key: VerifyingKey }

impl TrustStore {
    pub fn trust_production_key(
        &mut self,
        publisher: impl Into<String>,
        key_id: impl Into<String>,
        key: VerifyingKey,
    ) {
        self.production
            .insert((publisher.into(), key_id.into()), key);
    }

    pub fn trust_development_key(
        &mut self,
        publisher: impl Into<String>,
        key_id: impl Into<String>,
        key: VerifyingKey,
    ) {
        self.development
            .insert((publisher.into(), key_id.into()), key);
    }
    /// Restricts this authenticated root to exact package signing receipts. Unlisted
    /// digests never inherit its publisher keys; stored metadata must match the receipt.
    pub fn restrict_to_modules<'a>(&self, modules: impl IntoIterator<Item = (&'a ModuleManifest, &'a ModuleSignature)>) -> Result<Self, ArtifactError> {
        let mut authority = HashMap::new();
        for (manifest, signature) in modules {
            let key = self.production.get(&(signature.publisher.clone(), signature.key_id.clone())).ok_or_else(|| ArtifactError::SignatureVerification)?;
            let digest = module_manifest_signing_digest(manifest)?;
            if let Some(existing) = authority.insert(digest, ScopedModuleAuthority { signature: signature.clone(), key: *key }) {
                if existing.signature != *signature || existing.key != *key { return Err(ArtifactError::SignatureVerification); }
            }
        }
        Ok(Self { production: HashMap::new(), development: HashMap::new(), modules: Some(authority) })
    }
    pub fn extend_module_authority(&mut self, other: Self) -> Result<(), ArtifactError> {
        let destination = self.modules.as_mut().ok_or(ArtifactError::SignatureVerification)?;
        for (digest, entry) in other.modules.ok_or(ArtifactError::SignatureVerification)? {
            if let Some(existing) = destination.get(&digest) {
                if existing.signature != entry.signature || existing.key != entry.key { return Err(ArtifactError::SignatureVerification); }
            } else { destination.insert(digest, entry); }
        }
        Ok(())
    }

    fn verify(
        &self,
        environment: RuntimeEnvironment,
        manifest: &ModuleManifest,
        metadata: &ModuleSignature,
        digest: &[u8; 32],
    ) -> Result<(), ArtifactError> {
        if environment == RuntimeEnvironment::Production {
            validate_production_origin(manifest)?;
        }
        if metadata.schema_version != 1
            || metadata.algorithm != SignatureAlgorithm::Ed25519
            || metadata.publisher != manifest.publisher
        {
            return Err(ArtifactError::SignaturePublisherMismatch);
        }
        let identity = (metadata.publisher.clone(), metadata.key_id.clone());
        let key = if let Some(modules) = &self.modules {
            let receipt = modules.get(digest).ok_or(ArtifactError::SignatureVerification)?;
            if receipt.signature != *metadata { return Err(ArtifactError::SignatureVerification); }
            Some(&receipt.key)
        } else {
            self.production.get(&identity).or_else(|| {
                (environment == RuntimeEnvironment::Development).then(|| self.development.get(&identity)).flatten()
            })
        };
        let key = key.ok_or_else(|| ArtifactError::UntrustedSigner {
            publisher: metadata.publisher.clone(),
            key_id: metadata.key_id.clone(),
            environment,
        })?;
        let signature_bytes = BASE64
            .decode(metadata.signature.as_bytes())
            .map_err(|_| ArtifactError::InvalidSignatureEncoding)?;
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|_| ArtifactError::InvalidSignatureEncoding)?;
        key.verify_strict(digest, &signature)
            .map_err(|_| ArtifactError::SignatureVerification)
    }
}

#[derive(Debug, Clone)]
pub struct InstalledModule {
    pub digest: String,
    pub root: PathBuf,
    pub manifest: ModuleManifest,
    pub signature: ModuleSignature,
}

impl InstalledModule {
    pub fn entry_path(&self, entry: &str) -> Result<PathBuf, ArtifactError> {
        let relative = checked_relative_path(entry)?;
        let path = self.root.join(relative);
        path.is_file()
            .then_some(path)
            .ok_or_else(|| ArtifactError::NotFound(entry.to_owned()))
    }
}

#[derive(Debug)]
pub struct ModuleLease {
    module: InstalledModule,
    _lease: ArtifactLease,
}

impl ModuleLease {
    pub fn module(&self) -> &InstalledModule {
        &self.module
    }
}

#[derive(Debug)]
pub struct ArtifactGrant {
    pub descriptor: ArtifactGrantDescriptor,
    issued_descriptor: ArtifactGrantDescriptor,
    path: PathBuf,
    _lease: ArtifactLease,
}

impl ArtifactGrant {
    pub fn open_read(&self) -> Result<File, ArtifactError> {
        if self.descriptor != self.issued_descriptor
            || self.descriptor.access != ArtifactGrantAccess::Read
        {
            return Err(ArtifactError::InvalidFile {
                path: self.path.display().to_string(),
                message: "artifact grant descriptor was altered or is not readable".to_owned(),
            });
        }
        Ok(File::open(&self.path)?)
    }
}

#[derive(Debug)]
struct ArtifactLease {
    key: String,
    leases: Arc<Mutex<HashMap<String, usize>>>,
}

impl Drop for ArtifactLease {
    fn drop(&mut self) {
        let mut leases = self
            .leases
            .lock()
            .expect("artifact lease registry poisoned");
        if let Some(count) = leases.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                leases.remove(&self.key);
            }
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct GarbageCollectionReport {
    pub removed: Vec<String>,
    pub leased: Vec<String>,
    pub retained: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
    modules: PathBuf,
    blobs: PathBuf,
    staging: PathBuf,
    environment: RuntimeEnvironment,
    target: String,
    host_api: String,
    trust: TrustStore,
    leases: Arc<Mutex<HashMap<String, usize>>>,
}

impl ArtifactStore {
    pub fn from_layout(
        layout: &RuntimeLayout,
        environment: RuntimeEnvironment,
        target: impl Into<String>,
        host_api: impl Into<String>,
        trust: TrustStore,
    ) -> Result<Self, ArtifactError> {
        if !layout.initialized {
            return Err(ArtifactError::UninitializedLayout);
        }
        let root = layout.artifacts_dir.clone();
        let store = Self {
            modules: root.join("modules").join("sha256"),
            blobs: root.join("blobs").join("sha256"),
            staging: root.join("staging"),
            root,
            environment,
            target: target.into(),
            host_api: host_api.into(),
            trust,
            leases: Arc::new(Mutex::new(HashMap::new())),
        };
        fs::create_dir_all(&store.modules)?;
        fs::create_dir_all(&store.blobs)?;
        fs::create_dir_all(&store.staging)?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Changes signature authority for an authenticated release while preserving the exact
    /// store paths and live lease registry. Callers must derive trust from a verified root.
    pub fn with_trust(&self, trust: TrustStore) -> Self {
        let mut store = self.clone();
        store.trust = trust;
        store
    }

    pub fn publish_module(
        &self,
        source: &Path,
        manifest: &ModuleManifest,
        signature: &ModuleSignature,
    ) -> Result<InstalledModule, ArtifactError> {
        self.validate_manifest(manifest)?;
        let digest_bytes = module_signing_digest(source, manifest)?;
        self.trust
            .verify(self.environment, manifest, signature, &digest_bytes)?;
        let digest = format_digest(digest_bytes);
        let destination = self.module_path(&digest)?;
        if destination.exists() {
            return self.load_module(&digest);
        }

        let temporary = self.staging.join(format!("module-{}", Uuid::new_v4()));
        fs::create_dir(&temporary)?;
        let publish_result = (|| {
            for file in &manifest.files {
                let relative = checked_relative_path(&file.path)?;
                let from = source.join(&relative);
                let to = temporary.join(&relative);
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                copy_and_sync(&from, &to)?;
                set_payload_permissions(&to, file.executable)?;
            }
            write_json_file(&temporary.join(MODULE_MANIFEST_FILE), manifest)?;
            write_json_file(&temporary.join(MODULE_SIGNATURE_FILE), signature)?;
            match fs::rename(&temporary, &destination) {
                Ok(()) => {
                    make_tree_read_only(&destination)?;
                    sync_directory(destination.parent().expect("module path has parent"))?;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    fs::remove_dir_all(&temporary)?;
                }
                Err(error) => return Err(error.into()),
            }
            self.load_module(&digest)
        })();
        if publish_result.is_err() && temporary.exists() {
            let _ = make_tree_writable(&temporary);
            let _ = fs::remove_dir_all(&temporary);
        }
        publish_result
    }

    pub fn load_module(&self, digest: &str) -> Result<InstalledModule, ArtifactError> {
        let root = self.module_path(digest)?;
        if !root.is_dir() {
            return Err(ArtifactError::NotFound(digest.to_owned()));
        }
        let manifest: ModuleManifest = read_json_file(&root.join(MODULE_MANIFEST_FILE))?;
        let signature: ModuleSignature = read_json_file(&root.join(MODULE_SIGNATURE_FILE))?;
        self.validate_manifest(&manifest)?;
        let digest_bytes = module_signing_digest_installed(&root, &manifest)?;
        let actual = format_digest(digest_bytes);
        if actual != digest {
            return Err(ArtifactError::InvalidDigest(digest.to_owned()));
        }
        self.trust
            .verify(self.environment, &manifest, &signature, &digest_bytes)?;
        Ok(InstalledModule {
            digest: digest.to_owned(),
            root,
            manifest,
            signature,
        })
    }

    pub fn acquire_module(&self, digest: &str) -> Result<ModuleLease, ArtifactError> {
        let module = self.load_module(digest)?;
        let lease = self.acquire_lease(format!("module:{digest}"));
        Ok(ModuleLease {
            module,
            _lease: lease,
        })
    }

    pub fn publish_blob_file(
        &self,
        source: &Path,
        media_type: Option<String>,
    ) -> Result<ArtifactRef, ArtifactError> {
        let mut source = File::open(source)?;
        self.publish_blob_reader(&mut source, media_type)
    }

    pub fn publish_blob_reader(
        &self,
        source: &mut impl Read,
        media_type: Option<String>,
    ) -> Result<ArtifactRef, ArtifactError> {
        let temporary = self.staging.join(format!("blob-{}", Uuid::new_v4()));
        let (digest_bytes, size) = copy_reader_hash_and_sync(source, &temporary)?;
        let digest = format_digest(digest_bytes);
        let destination = self.blob_path(&digest)?;
        set_payload_permissions(&temporary, false)?;
        match fs::rename(&temporary, &destination) {
            Ok(()) => sync_directory(destination.parent().expect("blob path has parent"))?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary)?;
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error.into());
            }
        }
        Ok(ArtifactRef {
            digest,
            size,
            verified: true,
            media_type,
        })
    }

    pub fn grant_blob(&self, artifact: &ArtifactRef) -> Result<ArtifactGrant, ArtifactError> {
        if !artifact.verified {
            return Err(ArtifactError::UnverifiedArtifact(artifact.digest.clone()));
        }
        let path = self.blob_path(&artifact.digest)?;
        let metadata =
            fs::metadata(&path).map_err(|error| map_not_found(error, &artifact.digest))?;
        if metadata.len() != artifact.size {
            return Err(ArtifactError::InvalidFile {
                path: path.display().to_string(),
                message: format!("expected {} bytes, found {}", artifact.size, metadata.len()),
            });
        }
        let lease = self.acquire_lease(format!("blob:{}", artifact.digest));
        let descriptor = ArtifactGrantDescriptor {
            grant_id: Uuid::new_v4().to_string(),
            digest: artifact.digest.clone(),
            size: artifact.size,
            path: path.to_string_lossy().into_owned(),
            access: ArtifactGrantAccess::Read,
        };
        Ok(ArtifactGrant {
            descriptor: descriptor.clone(),
            issued_descriptor: descriptor,
            path,
            _lease: lease,
        })
    }

    pub fn collect_garbage(
        &self,
        retained_digests: &HashSet<String>,
    ) -> Result<GarbageCollectionReport, ArtifactError> {
        let leases = self
            .leases
            .lock()
            .expect("artifact lease registry poisoned");
        let mut report = GarbageCollectionReport::default();
        collect_kind(
            &self.modules,
            true,
            "module",
            retained_digests,
            &leases,
            &mut report,
        )?;
        collect_kind(
            &self.blobs,
            false,
            "blob",
            retained_digests,
            &leases,
            &mut report,
        )?;
        report.removed.sort();
        report.leased.sort();
        report.retained.sort();
        Ok(report)
    }

    fn validate_manifest(&self, manifest: &ModuleManifest) -> Result<(), ArtifactError> {
        if manifest.schema_version != 1 {
            return Err(ArtifactError::InvalidManifest(format!(
                "unsupported schema version {}",
                manifest.schema_version
            )));
        }
        if manifest.target != self.target {
            return Err(ArtifactError::TargetMismatch {
                expected: self.target.clone(),
                actual: manifest.target.clone(),
            });
        }
        if manifest.host_api != self.host_api {
            return Err(ArtifactError::HostApiMismatch {
                expected: self.host_api.clone(),
                actual: manifest.host_api.clone(),
            });
        }
        if manifest.id.trim().is_empty()
            || manifest.version.trim().is_empty()
            || manifest.publisher.trim().is_empty()
        {
            return Err(ArtifactError::InvalidManifest(
                "id, version, and publisher must be non-empty".to_owned(),
            ));
        }
        let files: HashSet<&str> = manifest
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        if files.len() != manifest.files.len() {
            return Err(ArtifactError::InvalidManifest(
                "module file paths must be unique".to_owned(),
            ));
        }
        let mut provided = HashSet::new();
        for contract in &manifest.provides {
            if !provided.insert((&contract.contract_id, contract.version)) {
                return Err(ArtifactError::InvalidManifest(format!(
                    "duplicate provided contract {}@{}",
                    contract.contract_id, contract.version
                )));
            }
        }
        for component in &manifest.components {
            checked_relative_path(&component.entry)?;
            let file = manifest
                .files
                .iter()
                .find(|file| file.path == component.entry)
                .ok_or_else(|| {
                    ArtifactError::InvalidManifest(format!(
                        "component entry {} is absent from files",
                        component.entry
                    ))
                })?;
            if component.runtime == "worker" && !file.executable {
                return Err(ArtifactError::InvalidManifest(format!(
                    "worker entry {} must be executable",
                    component.entry
                )));
            }
        }
        Ok(())
    }

    fn module_path(&self, digest: &str) -> Result<PathBuf, ArtifactError> {
        Ok(self.modules.join(digest_hex(digest)?))
    }

    fn blob_path(&self, digest: &str) -> Result<PathBuf, ArtifactError> {
        Ok(self.blobs.join(digest_hex(digest)?))
    }

    fn acquire_lease(&self, key: String) -> ArtifactLease {
        let mut leases = self
            .leases
            .lock()
            .expect("artifact lease registry poisoned");
        *leases.entry(key.clone()).or_insert(0) += 1;
        ArtifactLease {
            key,
            leases: Arc::clone(&self.leases),
        }
    }
}

/// Development-origin packages are never executable under production trust,
/// including when an operator accidentally authorizes their signing key.
pub fn validate_production_origin(manifest: &ModuleManifest) -> Result<(), ArtifactError> {
    if !manifest.contributes.is_object() {
        return Err(ArtifactError::InvalidManifest("contributes must be an object".to_owned()));
    }
    match manifest.contributes.get("origin") {
        None => Ok(()),
        Some(serde_json::Value::String(origin)) if origin == "production" => Ok(()),
        _ => Err(ArtifactError::InvalidManifest(
            "non-production package origin is forbidden under production trust".to_owned(),
        )),
    }
}

pub fn module_signing_digest(
    source: &Path,
    manifest: &ModuleManifest,
) -> Result<[u8; 32], ArtifactError> {
    validate_payload(source, manifest, false)?;
    module_manifest_signing_digest(manifest)
}

fn module_signing_digest_installed(
    source: &Path,
    manifest: &ModuleManifest,
) -> Result<[u8; 32], ArtifactError> {
    validate_payload(source, manifest, true)?;
    module_manifest_signing_digest(manifest)
}

/// Computes the signed manifest identity. This does not verify payload bytes.
/// Call `module_signing_digest` before publishing or executing a payload.
pub fn module_manifest_signing_digest(manifest: &ModuleManifest) -> Result<[u8; 32], ArtifactError> {
    let mut canonical = manifest.clone();
    canonical
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    let manifest_bytes = serde_json::to_vec(&canonical)?;
    let mut hasher = Sha256::new();
    hasher.update(PACKAGE_DOMAIN);
    hasher.update((manifest_bytes.len() as u64).to_be_bytes());
    hasher.update(&manifest_bytes);
    Ok(hasher.finalize().into())
}

fn validate_payload(
    source: &Path,
    manifest: &ModuleManifest,
    installed: bool,
) -> Result<(), ArtifactError> {
    let mut expected = BTreeSet::new();
    for file in &manifest.files {
        let relative = checked_relative_path(&file.path)?;
        if !expected.insert(file.path.clone()) {
            return Err(ArtifactError::InvalidFile {
                path: file.path.clone(),
                message: "duplicate path".to_owned(),
            });
        }
        let path = source.join(relative);
        let metadata = fs::symlink_metadata(&path).map_err(|error| ArtifactError::InvalidFile {
            path: file.path.clone(),
            message: error.to_string(),
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ArtifactError::InvalidFile {
                path: file.path.clone(),
                message: "payload entries must be regular files".to_owned(),
            });
        }
        if metadata.len() != file.size {
            return Err(ArtifactError::InvalidFile {
                path: file.path.clone(),
                message: format!("expected {} bytes, found {}", file.size, metadata.len()),
            });
        }
        let actual = hash_file(&path)?;
        if format_digest(actual) != file.sha256 {
            return Err(ArtifactError::InvalidFile {
                path: file.path.clone(),
                message: "SHA-256 mismatch".to_owned(),
            });
        }
    }
    let mut actual = BTreeSet::new();
    collect_payload_files(source, source, installed, &mut actual)?;
    if actual != expected {
        return Err(ArtifactError::FileListMismatch);
    }
    Ok(())
}

fn collect_payload_files(
    root: &Path,
    directory: &Path,
    installed: bool,
    files: &mut BTreeSet<String>,
) -> Result<(), ArtifactError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(ArtifactError::InvalidFile {
                path: path.display().to_string(),
                message: "symlinks are not allowed".to_owned(),
            });
        }
        if file_type.is_dir() {
            collect_payload_files(root, &path, installed, files)?;
        } else if file_type.is_file() {
            let relative = path.strip_prefix(root).expect("walk remains beneath root");
            let relative = relative_path_string(relative)?;
            if installed && (relative == MODULE_MANIFEST_FILE || relative == MODULE_SIGNATURE_FILE)
            {
                continue;
            }
            files.insert(relative);
        } else {
            return Err(ArtifactError::InvalidFile {
                path: path.display().to_string(),
                message: "payload entries must be regular files".to_owned(),
            });
        }
    }
    Ok(())
}

fn checked_relative_path(value: &str) -> Result<PathBuf, ArtifactError> {
    if value.is_empty() || value.contains('\\') {
        return Err(ArtifactError::InvalidFile {
            path: value.to_owned(),
            message: "path must use non-empty forward-slash relative form".to_owned(),
        });
    }
    let path = Path::new(value);
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(path.to_path_buf())
    } else {
        Err(ArtifactError::InvalidFile {
            path: value.to_owned(),
            message: "absolute, parent, and current-directory components are forbidden".to_owned(),
        })
    }
}

fn relative_path_string(path: &Path) -> Result<String, ArtifactError> {
    let mut result = String::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(ArtifactError::InvalidFile {
                path: path.display().to_string(),
                message: "non-normal path component".to_owned(),
            });
        };
        let text = value.to_str().ok_or_else(|| ArtifactError::InvalidFile {
            path: path.display().to_string(),
            message: "path is not valid UTF-8".to_owned(),
        })?;
        if !result.is_empty() {
            result.push('/');
        }
        result.push_str(text);
    }
    Ok(result)
}

fn format_digest(bytes: [u8; 32]) -> String {
    let mut value = String::with_capacity(71);
    value.push_str("sha256:");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    value
}

fn digest_hex(digest: &str) -> Result<&str, ArtifactError> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(ArtifactError::InvalidDigest(digest.to_owned()));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ArtifactError::InvalidDigest(digest.to_owned()));
    }
    Ok(hex)
}

fn hash_file(path: &Path) -> Result<[u8; 32], ArtifactError> {
    let mut input = File::open(path)?;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().into())
}

fn copy_reader_hash_and_sync(
    input: &mut impl Read,
    destination: &Path,
) -> Result<([u8; 32], u64), ArtifactError> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| ArtifactError::InvalidFile {
                path: destination.display().to_string(),
                message: "blob size overflow".to_owned(),
            })?;
    }
    output.sync_all()?;
    Ok((hasher.finalize().into(), size))
}

fn copy_and_sync(source: &Path, destination: &Path) -> Result<(), ArtifactError> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(())
}

fn write_json_file<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), ArtifactError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    output.write_all(&bytes)?;
    output.sync_all()?;
    set_payload_permissions(path, false)
}

fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ArtifactError> {
    Ok(serde_json::from_reader(File::open(path)?)?)
}

fn set_payload_permissions(path: &Path, executable: bool) -> Result<(), ArtifactError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if executable { 0o555 } else { 0o444 }),
        )?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions)?;
        let _ = executable;
    }
    Ok(())
}

fn make_tree_read_only(root: &Path) -> Result<(), ArtifactError> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            make_tree_read_only(&path)?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o555))?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(root)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(root, permissions)?;
    }
    Ok(())
}

fn make_tree_writable(root: &Path) -> Result<(), ArtifactError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(windows)]
    {
        let mut permissions = fs::metadata(root)?.permissions();
        // On Windows this only clears FILE_ATTRIBUTE_READONLY.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(root, permissions)?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut permissions = fs::metadata(root)?.permissions();
        permissions.set_readonly(false);
        fs::set_permissions(root, permissions)?;
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            make_tree_writable(&path)?;
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
            }
            #[cfg(windows)]
            {
                let mut permissions = fs::metadata(&path)?.permissions();
                // On Windows this only clears FILE_ATTRIBUTE_READONLY.
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                fs::set_permissions(&path, permissions)?;
            }
            #[cfg(not(any(unix, windows)))]
            {
                let mut permissions = fs::metadata(&path)?.permissions();
                permissions.set_readonly(false);
                fs::set_permissions(&path, permissions)?;
            }
        }
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), ArtifactError> {
    #[cfg(unix)]
    if let Err(error) = File::open(path).and_then(|directory| directory.sync_all()) {
        // Some local and network filesystems reject fsync on directories. Payload files are
        // already synced and rename remains atomic; only the power-loss durability hint is lost.
        if !matches!(
            error.kind(),
            io::ErrorKind::PermissionDenied
                | io::ErrorKind::InvalidInput
                | io::ErrorKind::Unsupported
        ) {
            return Err(error.into());
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn collect_kind(
    directory: &Path,
    directories: bool,
    kind: &str,
    retained: &HashSet<String>,
    leases: &HashMap<String, usize>,
    report: &mut GarbageCollectionReport,
) -> Result<(), ArtifactError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() != 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            continue;
        }
        let digest = format!("sha256:{name}");
        let key = format!("{kind}:{digest}");
        if retained.contains(&digest) {
            report.retained.push(key);
        } else if leases.contains_key(&key) {
            report.leased.push(key);
        } else {
            if directories {
                make_tree_writable(&entry.path())?;
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
            report.removed.push(key);
        }
    }
    Ok(())
}

fn map_not_found(error: io::Error, digest: &str) -> ArtifactError {
    if error.kind() == io::ErrorKind::NotFound {
        ArtifactError::NotFound(digest.to_owned())
    } else {
        ArtifactError::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, fs};

    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use ed25519_dalek::{Signer, SigningKey};
    use mintcat_runtime_kernel::{RuntimeEnvironment, RuntimePaths};
    use mintcat_sdk::{
        ModuleActivation, ModuleComponent, ModuleFile, ModuleManifest, ModuleSignature,
        ModuleState, SignatureAlgorithm,
    };
    use sha2::Digest as _;
    use tempfile::tempdir;

    use super::{module_signing_digest, ArtifactError, ArtifactStore, TrustStore};

    fn fixture_manifest(target: &str, payload: &[u8]) -> ModuleManifest {
        let digest = super::format_digest(sha2::Sha256::digest(payload).into());
        ModuleManifest {
            schema_version: 1,
            id: "fixture.worker".to_owned(),
            version: "1.0.0".to_owned(),
            publisher: "mintcat.test".to_owned(),
            target: target.to_owned(),
            host_api: "1".to_owned(),
            components: vec![ModuleComponent {
                kind: "service".to_owned(),
                entry: "worker".to_owned(),
                runtime: "worker".to_owned(),
                state_model: "stateless".to_owned(),
            }],
            provides: Vec::new(),
            requires: Vec::new(),
            permissions: Vec::new(),
            contributes: serde_json::json!({}),
            state: ModuleState {
                schema_version: 1,
                readable_range: "1".to_owned(),
                writable_range: "1".to_owned(),
                rollback_before_write_only: Some(true),
            },
            activation: ModuleActivation {
                trigger: "on-demand".to_owned(),
                drain_policy: "finish-requests".to_owned(),
            },
            files: vec![ModuleFile {
                path: "worker".to_owned(),
                size: payload.len() as u64,
                sha256: digest,
                executable: true,
            }],
        }
    }

    fn signed_fixture(
        environment: RuntimeEnvironment,
        development_key: bool,
    ) -> (
        tempfile::TempDir,
        ArtifactStore,
        ModuleManifest,
        ModuleSignature,
    ) {
        let sandbox = tempdir().unwrap();
        let layout = RuntimePaths::new(sandbox.path().join("data"))
            .unwrap()
            .initialize(environment)
            .unwrap();
        let source = sandbox.path().join("source");
        fs::create_dir(&source).unwrap();
        let payload = b"worker-binary";
        fs::write(source.join("worker"), payload).unwrap();
        let manifest = fixture_manifest("test-target", payload);
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let digest = module_signing_digest(&source, &manifest).unwrap();
        let signature = ModuleSignature {
            schema_version: 1,
            publisher: manifest.publisher.clone(),
            key_id: "fixture-key".to_owned(),
            algorithm: SignatureAlgorithm::Ed25519,
            signature: BASE64.encode(signing_key.sign(&digest).to_bytes()),
        };
        let mut trust = TrustStore::default();
        if development_key {
            trust.trust_development_key(
                &manifest.publisher,
                &signature.key_id,
                signing_key.verifying_key(),
            );
        } else {
            trust.trust_production_key(
                &manifest.publisher,
                &signature.key_id,
                signing_key.verifying_key(),
            );
        }
        let store =
            ArtifactStore::from_layout(&layout, environment, "test-target", "1", trust).unwrap();
        (sandbox, store, manifest, signature)
    }

    #[test]
    fn publishes_immutable_signed_modules_and_leases_block_gc() {
        let (sandbox, store, manifest, signature) = signed_fixture(RuntimeEnvironment::Test, false);
        let source = sandbox.path().join("source");
        let installed = store
            .publish_module(&source, &manifest, &signature)
            .unwrap();
        assert_eq!(installed.manifest, manifest);
        assert!(installed.entry_path("worker").unwrap().is_file());

        let lease = store.acquire_module(&installed.digest).unwrap();
        let report = store.collect_garbage(&HashSet::new()).unwrap();
        assert_eq!(report.leased, vec![format!("module:{}", installed.digest)]);
        assert!(lease.module().root.exists());
        drop(lease);

        let report = store.collect_garbage(&HashSet::new()).unwrap();
        assert_eq!(report.removed, vec![format!("module:{}", installed.digest)]);
        assert!(!installed.root.exists());
    }

    #[test]
    fn rejects_wrong_target_and_tampered_signatures() {
        let (sandbox, store, mut manifest, mut signature) =
            signed_fixture(RuntimeEnvironment::Test, false);
        manifest.target = "wrong-target".to_owned();
        let error = store
            .publish_module(&sandbox.path().join("source"), &manifest, &signature)
            .unwrap_err();
        assert!(matches!(error, ArtifactError::TargetMismatch { .. }));

        manifest.target = "test-target".to_owned();
        signature.signature = BASE64.encode([0_u8; 64]);
        let error = store
            .publish_module(&sandbox.path().join("source"), &manifest, &signature)
            .unwrap_err();
        assert!(matches!(error, ArtifactError::SignatureVerification));
    }

    #[test]
    fn rejects_unlisted_and_modified_payload_files() {
        let (sandbox, store, manifest, signature) = signed_fixture(RuntimeEnvironment::Test, false);
        let source = sandbox.path().join("source");
        fs::write(source.join("unlisted"), b"not in manifest").unwrap();
        let error = store
            .publish_module(&source, &manifest, &signature)
            .unwrap_err();
        assert!(matches!(error, ArtifactError::FileListMismatch));

        fs::remove_file(source.join("unlisted")).unwrap();
        fs::write(source.join("worker"), b"worker-binarx").unwrap();
        let error = store
            .publish_module(&source, &manifest, &signature)
            .unwrap_err();
        assert!(matches!(error, ArtifactError::InvalidFile { .. }));
    }

    #[test]
    fn development_keys_are_not_trusted_in_production() {
        let (sandbox, development_store, manifest, signature) =
            signed_fixture(RuntimeEnvironment::Development, true);
        development_store
            .publish_module(&sandbox.path().join("source"), &manifest, &signature)
            .unwrap();
        let production_layout = RuntimePaths::new(sandbox.path().join("production"))
            .unwrap()
            .initialize(RuntimeEnvironment::Production)
            .unwrap();
        let store = ArtifactStore::from_layout(
            &production_layout,
            RuntimeEnvironment::Production,
            "test-target",
            "1",
            TrustStore::default(),
        )
        .unwrap();
        let error = store
            .publish_module(&sandbox.path().join("source"), &manifest, &signature)
            .unwrap_err();
        assert!(matches!(error, ArtifactError::UntrustedSigner { .. }));
    }
}
