//! Transport for already-authorized host grants. These values are worker-protocol
//! inputs, never application-command parameters or a source of publisher trust.
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerModuleGrant {
    pub digest: String,
    pub root: PathBuf,
    pub manifest: ModuleManifest,
    pub signature: ModuleSignature,
    pub verifying_key: [u8; 32],
    pub environment: RuntimeEnvironment,
}

impl ArtifactStore {
    pub fn module_worker_grant(&self, lease: &ModuleLease) -> Result<WorkerModuleGrant, ArtifactError> {
        let module = lease.module();
        let identity = (module.signature.publisher.clone(), module.signature.key_id.clone());
        let key = self.trust.production.get(&identity).or_else(|| {
            (self.environment == RuntimeEnvironment::Development)
                .then(|| self.trust.development.get(&identity))
                .flatten()
        }).ok_or_else(|| ArtifactError::UntrustedSigner {
            publisher: identity.0.clone(), key_id: identity.1.clone(), environment: self.environment,
        })?;
        Ok(WorkerModuleGrant {
            digest: module.digest.clone(), root: module.root.clone(),
            manifest: module.manifest.clone(), signature: module.signature.clone(),
            verifying_key: key.to_bytes(),
            environment: self.environment,
        })
    }
}

impl ArtifactStore {
    /// Authenticate a producer-owned file, never an application-supplied path.
    /// Native workers are trusted code; this boundary is not an OS sandbox.
    pub fn grant_produced(
        &self,
        workspace: &Path,
        descriptor: &ArtifactGrantDescriptor,
    ) -> Result<ArtifactGrant, ArtifactError> {
        digest_hex(&descriptor.digest)?;
        let root = fs::canonicalize(workspace)?;
        let path = PathBuf::from(&descriptor.path);
        if descriptor.access != ArtifactGrantAccess::Read || !path.is_absolute()
            || !path.starts_with(&root) || path == root
            || fs::canonicalize(&path)? != path {
            return Err(ArtifactError::InvalidFile {
                path: descriptor.path.clone(), message: "produced artifact escapes private workspace".into(),
            });
        }
        let mut ancestor = path.as_path();
        while ancestor != root {
            if fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
                return Err(ArtifactError::InvalidFile {
                    path: descriptor.path.clone(), message: "produced artifact contains a symlink".into(),
                });
            }
            ancestor = ancestor.parent().ok_or_else(|| ArtifactError::InvalidFile {
                path: descriptor.path.clone(), message: "produced artifact has no workspace parent".into(),
            })?;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.len() != descriptor.size
            || format_digest(hash_file(&path)?) != descriptor.digest {
            return Err(ArtifactError::InvalidFile {
                path: descriptor.path.clone(), message: "produced artifact bytes do not match digest/size".into(),
            });
        }
        let mut authenticated = descriptor.clone();
        authenticated.grant_id = Uuid::new_v4().to_string();
        Ok(ArtifactGrant {
            _lease: detached_lease(format!("produced:{}", authenticated.digest)),
            issued_descriptor: authenticated.clone(), descriptor: authenticated, path,
        })
    }

    /// Adopt verified broker bytes by same-volume hardlink publication. The
    /// private producer workspace remains host-owned until final publication.
    pub fn adopt_verified_grant(&self, grant: &ArtifactGrant) -> Result<ArtifactRef, ArtifactError> {
        let file = grant.open_read()?;
        if file.metadata()?.len() != grant.descriptor.size
            || format_digest(hash_file(&grant.path)?) != grant.descriptor.digest {
            return Err(ArtifactError::UnverifiedArtifact(grant.descriptor.digest.clone()));
        }
        let destination = self.blob_path(&grant.descriptor.digest)?;
        match fs::hard_link(&grant.path, &destination) {
            Ok(()) => {
                set_payload_permissions(&destination, false)?;
                sync_directory(destination.parent().expect("blob path has parent"))?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if fs::metadata(&destination)?.len() != grant.descriptor.size
                    || format_digest(hash_file(&destination)?) != grant.descriptor.digest {
                    return Err(ArtifactError::UnverifiedArtifact(grant.descriptor.digest.clone()));
                }
            }
            Err(error) if error.raw_os_error() == Some(18) => {
                let artifact = self.publish_blob_reader(&mut { file }, None)?;
                if artifact.digest != grant.descriptor.digest || artifact.size != grant.descriptor.size {
                    return Err(ArtifactError::UnverifiedArtifact(grant.descriptor.digest.clone()));
                }
                return Ok(artifact);
            }
            Err(error) => return Err(error.into()),
        }
        Ok(ArtifactRef {
            digest: grant.descriptor.digest.clone(), size: grant.descriptor.size,
            verified: true, media_type: None,
        })
    }
}

impl ModuleLease {
    /// The supervisor must authorize the package before transporting this grant.
    /// Verification here detects corrupt bytes; the transported key is not a new
    /// application trust root.
    pub fn from_worker_grant(grant: WorkerModuleGrant) -> Result<Self, ArtifactError> {
        let digest = module_signing_digest_installed(&grant.root, &grant.manifest)?;
        if format_digest(digest) != grant.digest {
            return Err(ArtifactError::InvalidDigest(grant.digest));
        }
        let key = VerifyingKey::from_bytes(&grant.verifying_key)
            .map_err(|_| ArtifactError::InvalidSignatureEncoding)?;
        let mut trust = TrustStore::default();
        trust.trust_production_key(&grant.signature.publisher, &grant.signature.key_id, key);
        trust.verify(grant.environment, &grant.manifest, &grant.signature, &digest)?;
        Ok(Self {
            _lease: detached_lease(format!("module:{}", grant.digest)),
            module: InstalledModule {
                digest: grant.digest, root: grant.root,
                manifest: grant.manifest, signature: grant.signature,
            },
        })
    }
}

impl ArtifactGrant {
    /// Resolve only descriptors in the supervisor-authenticated request envelope.
    pub fn from_worker_request(
        request: &mintcat_sdk::WorkerRequest,
        grant_id: &str,
    ) -> Result<Self, ArtifactError> {
        let descriptor = request.grants.iter().find(|grant| grant.grant_id == grant_id)
            .ok_or_else(|| ArtifactError::NotFound(grant_id.to_owned()))?.clone();
        if descriptor.access != ArtifactGrantAccess::Read {
            return Err(ArtifactError::UnverifiedArtifact(descriptor.digest));
        }
        digest_hex(&descriptor.digest)?;
        let path = PathBuf::from(&descriptor.path);
        let metadata = fs::symlink_metadata(&path)?;
        if !path.is_absolute() || metadata.file_type().is_symlink()
            || !metadata.is_file() || metadata.len() != descriptor.size {
            return Err(ArtifactError::InvalidFile {
                path: descriptor.path, message: "invalid host read grant".to_owned(),
            });
        }
        Ok(Self {
            _lease: detached_lease(format!("blob:{}", descriptor.digest)),
            issued_descriptor: descriptor.clone(), descriptor, path,
        })
    }
}

fn detached_lease(key: String) -> ArtifactLease {
    ArtifactLease { leases: Arc::new(Mutex::new(HashMap::from([(key.clone(), 1)]))), key }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_grants_reject_outside_symlink_and_corrupt_bytes() {
        let sandbox = tempfile::tempdir().unwrap();
        let layout = mintcat_runtime_kernel::RuntimePaths::new(sandbox.path().join("state"))
            .unwrap().initialize(RuntimeEnvironment::Test).unwrap();
        let store = ArtifactStore::from_layout(&layout, RuntimeEnvironment::Test, "test", "1", TrustStore::default()).unwrap();
        let workspace = sandbox.path().join("private");
        fs::create_dir(&workspace).unwrap();
        let workspace = fs::canonicalize(workspace).unwrap();
        let path = workspace.join("output");
        fs::write(&path, b"actual-output").unwrap();
        let descriptor = ArtifactGrantDescriptor {
            grant_id: "producer".into(), digest: format_digest(hash_file(&path).unwrap()),
            size: 13, path: path.to_string_lossy().into_owned(), access: ArtifactGrantAccess::Read,
        };
        let granted = store.grant_produced(&workspace, &descriptor).unwrap();
        assert_ne!(granted.descriptor.grant_id, descriptor.grant_id);
        let adopted = store.adopt_verified_grant(&granted).unwrap();
        assert_eq!(adopted.digest, descriptor.digest);
        assert_eq!(adopted.size, descriptor.size);
        let outside = sandbox.path().join("outside");
        fs::write(&outside, b"actual-output").unwrap();
        let mut forged = descriptor.clone();
        forged.path = outside.to_string_lossy().into_owned();
        assert!(store.grant_produced(&workspace, &forged).is_err());
        forged = descriptor.clone();
        forged.size += 1;
        assert!(store.grant_produced(&workspace, &forged).is_err());
        forged = descriptor.clone();
        forged.digest = format!("sha256:{}", "0".repeat(64));
        assert!(store.grant_produced(&workspace, &forged).is_err());
        #[cfg(unix)]
        {
            let link = workspace.join("linked");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            forged = descriptor.clone();
            forged.path = link.to_string_lossy().into_owned();
            assert!(store.grant_produced(&workspace, &forged).is_err());
        }
    }
}
