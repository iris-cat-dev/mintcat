use std::{collections::{BTreeMap, BTreeSet}, path::Path};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use mintcat_runtime_artifacts::TrustStore;
use mintcat_sdk::{ModuleManifest, ModuleSignature, SignatureAlgorithm};
use serde::{Deserialize, Serialize};
use crate::{canonical, read_json, require, sha256, Result};

const METADATA_DOMAIN: &[u8] = b"mintcat-release-metadata-v1\0";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetadataSignature { pub key_id: String, pub signature: String }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Signed<T> { pub signed: T, pub signatures: Vec<MetadataSignature> }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Role { pub key_ids: BTreeSet<String>, pub threshold: u32 }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetadataHeader {
    pub kind: String,
    pub schema_version: u64,
    pub version: u64,
    pub expires: u64,
}
impl MetadataHeader {
    pub fn validate(&self, kind: &str, now: u64) -> Result<()> {
        require(self.kind == kind && self.schema_version == 1 && self.version > 0, "invalid metadata role/schema/version")?;
        require(self.expires > now, format!("expired {kind} metadata"))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootMetadata {
    pub header: MetadataHeader,
    /// Keys are base64 raw Ed25519 public keys, indexed by SHA-256 key identity.
    pub keys: BTreeMap<String, String>,
    pub roles: BTreeMap<String, Role>,
    /// Explicit publisher namespace/key authorizations; never inferred from role keys.
    pub publishers: BTreeMap<String, BTreeMap<String, String>>,
}
#[derive(Debug, Clone)]
pub struct TrustedRoot { envelope: Signed<RootMetadata> }
impl TrustedRoot {
    pub fn envelope(&self) -> &Signed<RootMetadata> { &self.envelope }
    pub fn version(&self) -> u64 { self.envelope.signed.header.version }
    pub fn expires(&self) -> u64 { self.envelope.signed.header.expires }
    pub fn verify<T: Serialize>(&self, role: &str, value: &Signed<T>) -> Result<()> {
        verify_threshold(&self.envelope.signed, role, value)
    }
    pub fn rotate(&self, next: Signed<RootMetadata>, now: u64) -> Result<Self> {
        validate_root(&next.signed, now)?;
        require(next.signed.header.version == self.version().checked_add(1).ok_or_else(|| crate::ReleaseError::Rejected("root version overflow".into()))?, "root rotation must be exactly one version newer")?;
        // Both thresholds are required even when the sets have no overlapping key.
        self.verify("root", &next)?;
        verify_threshold(&next.signed, "root", &next)?;
        Ok(Self { envelope: next })
    }
    pub fn artifact_trust_store(&self) -> Result<TrustStore> {
        let mut store = TrustStore::default();
        for (publisher, keys) in &self.envelope.signed.publishers {
            for (id, encoded) in keys { store.trust_production_key(publisher, id, decode_key(encoded)?); }
        }
        Ok(store)
    }
    pub fn verify_package_signature(&self, manifest: &ModuleManifest, signature: &ModuleSignature, digest: &[u8; 32]) -> Result<()> {
        mintcat_runtime_artifacts::validate_production_origin(manifest)?;
        require(signature.schema_version == 1 && signature.algorithm == SignatureAlgorithm::Ed25519 && signature.publisher == manifest.publisher, "package signature identity mismatch")?;
        let key = self.envelope.signed.publishers.get(&manifest.publisher).and_then(|keys| keys.get(&signature.key_id)).ok_or_else(|| crate::ReleaseError::Rejected("package publisher/key not authorized by root".into()))?;
        let bytes = BASE64.decode(&signature.signature).map_err(|_| crate::ReleaseError::Rejected("invalid package signature encoding".into()))?;
        let signature = Signature::from_slice(&bytes).map_err(|_| crate::ReleaseError::Rejected("invalid package signature length".into()))?;
        decode_key(key)?.verify_strict(digest, &signature).map_err(|_| crate::ReleaseError::Rejected("invalid package signature".into()))
    }
}

/// The caller must provision this path from an authenticated, out-of-band root.
/// A valid self-signature does not authenticate an arbitrary downloaded root.
pub fn load_trust_root(path: &Path, now: u64) -> Result<TrustedRoot> {
    trust_pinned_root(read_root_pin(path)?, now)
}

/// Authenticates the out-of-band pin for root-chain recovery. Its expiry may
/// already have passed; downloaded metadata still needs a current final root.
pub fn load_root_rotation_anchor(path: &Path) -> Result<TrustedRoot> {
    trust_pinned_root(read_root_pin(path)?, 0)
}

fn read_root_pin(path: &Path) -> Result<Signed<RootMetadata>> {
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        require(std::fs::symlink_metadata(path)?.permissions().mode() & 0o022 == 0, "trust root file must not be writable by group/others")?;
    }
    read_json(path)
}
pub fn trust_pinned_root(envelope: Signed<RootMetadata>, now: u64) -> Result<TrustedRoot> {
    validate_root(&envelope.signed, now)?;
    verify_threshold(&envelope.signed, "root", &envelope)?;
    Ok(TrustedRoot { envelope })
}
pub fn key_id(key: &VerifyingKey) -> String { sha256(key.as_bytes()) }
pub fn encode_public_key(key: &VerifyingKey) -> String { BASE64.encode(key.as_bytes()) }
pub fn sign<T: Serialize>(value: T, keys: &[SigningKey]) -> Result<Signed<T>> {
    let bytes = signing_bytes(&value)?;
    let mut identities = BTreeSet::new();
    let mut signatures = Vec::new();
    for key in keys {
        let id = key_id(&key.verifying_key());
        require(identities.insert(id.clone()), "duplicate metadata signing key")?;
        signatures.push(MetadataSignature { key_id: id, signature: BASE64.encode(key.sign(&bytes).to_bytes()) });
    }
    signatures.sort_by(|a, b| a.key_id.cmp(&b.key_id));
    Ok(Signed { signed: value, signatures })
}
fn signing_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let body = canonical(value)?;
    let mut bytes = Vec::with_capacity(METADATA_DOMAIN.len() + 8 + body.len());
    bytes.extend_from_slice(METADATA_DOMAIN);
    bytes.extend_from_slice(&(body.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&body);
    Ok(bytes)
}
fn decode_key(encoded: &str) -> Result<VerifyingKey> {
    let bytes = BASE64.decode(encoded).map_err(|_| crate::ReleaseError::Rejected("invalid public key encoding".into()))?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| crate::ReleaseError::Rejected("invalid public key length".into()))?;
    let key = VerifyingKey::from_bytes(&bytes).map_err(|_| crate::ReleaseError::Rejected("invalid Ed25519 public key".into()))?;
    require(!key.is_weak(), "weak Ed25519 public key")?;
    Ok(key)
}
fn validate_root(root: &RootMetadata, now: u64) -> Result<()> {
    root.header.validate("root", now)?;
    require(root.roles.keys().map(String::as_str).collect::<BTreeSet<_>>() == BTreeSet::from(["root", "targets", "snapshot", "timestamp"]), "root must authorize exactly root/targets/snapshot/timestamp roles")?;
    for (id, encoded) in &root.keys { require(*id == key_id(&decode_key(encoded)?), "root key identity mismatch")?; }
    for role in root.roles.values() {
        require(role.threshold > 0 && role.threshold as usize <= role.key_ids.len(), "invalid signing threshold")?;
        require(role.key_ids.iter().all(|key| root.keys.contains_key(key)), "role references absent key")?;
    }
    for (publisher, keys) in &root.publishers {
        require(!publisher.trim().is_empty() && !keys.is_empty(), "invalid publisher authorization")?;
        let mut unique = BTreeSet::new();
        for (id, encoded) in keys {
            require(!id.is_empty(), "empty publisher key identity")?;
            require(unique.insert(key_id(&decode_key(encoded)?)), "duplicate publisher public key")?;
        }
    }
    Ok(())
}
fn verify_threshold<T: Serialize>(root: &RootMetadata, name: &str, envelope: &Signed<T>) -> Result<()> {
    let role = root.roles.get(name).ok_or_else(|| crate::ReleaseError::Rejected("unknown metadata role".into()))?;
    let bytes = signing_bytes(&envelope.signed)?;
    let mut seen = BTreeSet::new();
    let mut accepted = 0usize;
    for signature in &envelope.signatures {
        require(seen.insert(&signature.key_id), "duplicate signature key")?;
        if !role.key_ids.contains(&signature.key_id) { continue; }
        let encoded = root.keys.get(&signature.key_id).ok_or_else(|| crate::ReleaseError::Rejected("missing authorized key".into()))?;
        let bytes_sig = BASE64.decode(&signature.signature).map_err(|_| crate::ReleaseError::Rejected("invalid metadata signature encoding".into()))?;
        let signature = Signature::from_slice(&bytes_sig).map_err(|_| crate::ReleaseError::Rejected("invalid metadata signature length".into()))?;
        decode_key(encoded)?.verify_strict(&bytes, &signature).map_err(|_| crate::ReleaseError::Rejected("invalid metadata signature".into()))?;
        accepted += 1;
    }
    require(accepted >= role.threshold as usize, format!("{name} signature threshold not met"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetadataRef { pub version: u64, pub length: u64, pub digest: String }
impl MetadataRef {
    pub fn new<T: Serialize>(version: u64, envelope: &Signed<T>) -> Result<Self> {
        let bytes = canonical(envelope)?;
        Ok(Self { version, length: bytes.len() as u64, digest: sha256(&bytes) })
    }
    pub fn verify<T: Serialize>(&self, version: u64, envelope: &Signed<T>) -> Result<()> {
        require(*self == Self::new(version, envelope)?, "metadata version/digest/length mismatch")
    }
    /// Check transport bytes before decoding an individually fetched envelope.
    pub fn verify_bytes(&self, bytes: &[u8]) -> Result<()> {
        require(bytes.len() as u64 == self.length && sha256(bytes) == self.digest, "metadata transport digest/length mismatch")
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotMetadata { pub header: MetadataHeader, pub targets: BTreeMap<String, MetadataRef> }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TimestampMetadata { pub header: MetadataHeader, pub snapshot: MetadataRef }
