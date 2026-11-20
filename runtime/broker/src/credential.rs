use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::BrokerError;

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialRef(String);

impl CredentialRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn new() -> Self {
        Self(format!("credential:{}", Uuid::new_v4()))
    }
}

impl fmt::Debug for CredentialRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CredentialRef")
            .field(&self.0)
            .finish()
    }
}

pub struct SecretValue(Vec<u8>);

impl SecretValue {
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self, BrokerError> {
        let value = value.into();
        if value.is_empty() {
            return Err(BrokerError::InvalidRequest(
                "credential value cannot be empty".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    pub(crate) fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

pub trait CredentialStore: Send + Sync {
    fn store(&self, secret: &SecretValue) -> Result<CredentialRef, BrokerError>;
    fn resolve(&self, reference: &CredentialRef) -> Result<Option<SecretValue>, BrokerError>;
    fn delete(&self, reference: &CredentialRef) -> Result<(), BrokerError>;
}

#[derive(Clone)]
pub struct CredentialVault {
    store: Arc<dyn CredentialStore>,
}

impl CredentialVault {
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self { store }
    }

    pub fn store(&self, secret: SecretValue) -> Result<CredentialRef, BrokerError> {
        self.store.store(&secret)
    }

    pub fn delete(&self, reference: &CredentialRef) -> Result<(), BrokerError> {
        self.store.delete(reference)
    }

    pub(crate) fn resolver(&self) -> Arc<dyn CredentialStore> {
        Arc::clone(&self.store)
    }
}

pub struct OsCredentialStore {
    service: String,
}

// Fail compilation if keyring silently falls back to its in-memory mock backend.
#[cfg(target_os = "macos")]
const _: fn() -> std::any::TypeId = std::any::TypeId::of::<keyring::macos::MacCredential>;
#[cfg(target_os = "windows")]
const _: fn() -> std::any::TypeId = std::any::TypeId::of::<keyring::windows::WinCredential>;
#[cfg(target_os = "linux")]
const _: fn() -> std::any::TypeId = std::any::TypeId::of::<keyring::secret_service::SsCredential>;

#[cfg(any(target_os = "windows", test))]
const WINDOWS_MAX_CHUNK_BYTES: usize = 2048;
#[cfg(any(target_os = "windows", test))]
const CHUNK_MANIFEST_PREFIX: &[u8] = b"\0mintcat-cred-chunks:v1:";
#[cfg(any(target_os = "windows", test))]
const MAX_CREDENTIAL_CHUNKS: usize = 64;

#[cfg(any(target_os = "windows", test))]
trait SecretSlotBackend {
    fn set_slot(&self, key: &str, value: &[u8]) -> Result<(), BrokerError>;
    fn get_slot(&self, key: &str) -> Result<Option<Vec<u8>>, BrokerError>;
    fn delete_slot(&self, key: &str) -> Result<(), BrokerError>;
}

#[cfg(any(target_os = "windows", test))]
fn chunk_key(reference: &CredentialRef, index: usize) -> String {
    format!("{}.part.{index}", reference.as_str())
}

#[cfg(any(target_os = "windows", test))]
fn parse_chunk_manifest(bytes: &[u8]) -> Result<Option<usize>, BrokerError> {
    let Some(rest) = bytes.strip_prefix(CHUNK_MANIFEST_PREFIX) else {
        return Ok(None);
    };
    let count = std::str::from_utf8(rest)
        .ok()
        .and_then(|text| text.parse::<usize>().ok())
        .filter(|count| (1..=MAX_CREDENTIAL_CHUNKS).contains(count))
        .ok_or(BrokerError::CredentialStore)?;
    Ok(Some(count))
}

#[cfg(any(target_os = "windows", test))]
fn store_chunked(
    backend: &impl SecretSlotBackend,
    secret: &SecretValue,
    max_chunk_bytes: usize,
) -> Result<CredentialRef, BrokerError> {
    if max_chunk_bytes == 0 {
        return Err(BrokerError::CredentialStore);
    }
    let reference = CredentialRef::new();
    let raw = secret.expose();
    if raw.len() <= max_chunk_bytes && !raw.starts_with(CHUNK_MANIFEST_PREFIX) {
        backend.set_slot(reference.as_str(), raw)?;
        return Ok(reference);
    }
    let chunks: Vec<&[u8]> = raw.chunks(max_chunk_bytes).collect();
    if chunks.is_empty() || chunks.len() > MAX_CREDENTIAL_CHUNKS {
        return Err(BrokerError::CredentialStore);
    }
    for (index, chunk) in chunks.iter().enumerate() {
        if let Err(error) = backend.set_slot(&chunk_key(&reference, index), chunk) {
            for written in 0..index {
                let _ = backend.delete_slot(&chunk_key(&reference, written));
            }
            return Err(error);
        }
    }
    let mut manifest = Vec::with_capacity(CHUNK_MANIFEST_PREFIX.len() + 8);
    manifest.extend_from_slice(CHUNK_MANIFEST_PREFIX);
    manifest.extend_from_slice(chunks.len().to_string().as_bytes());
    if let Err(error) = backend.set_slot(reference.as_str(), &manifest) {
        for written in 0..chunks.len() {
            let _ = backend.delete_slot(&chunk_key(&reference, written));
        }
        return Err(error);
    }
    Ok(reference)
}

#[cfg(any(target_os = "windows", test))]
fn resolve_chunked(
    backend: &impl SecretSlotBackend,
    reference: &CredentialRef,
) -> Result<Option<SecretValue>, BrokerError> {
    let Some(mut primary) = backend.get_slot(reference.as_str())? else {
        return Ok(None);
    };
    let chunk_count = match parse_chunk_manifest(&primary) {
        Ok(Some(count)) => {
            primary.fill(0);
            count
        }
        Ok(None) => return Ok(Some(SecretValue::new(primary)?)),
        Err(error) => {
            primary.fill(0);
            return Err(error);
        }
    };
    let mut assembled = Vec::new();
    for index in 0..chunk_count {
        match backend.get_slot(&chunk_key(reference, index)) {
            Ok(Some(mut chunk)) => {
                assembled.extend_from_slice(&chunk);
                chunk.fill(0);
            }
            Ok(None) | Err(_) => {
                assembled.fill(0);
                return Err(BrokerError::CredentialStore);
            }
        }
    }
    SecretValue::new(assembled).map(Some)
}

#[cfg(any(target_os = "windows", test))]
fn delete_chunked(
    backend: &impl SecretSlotBackend,
    reference: &CredentialRef,
) -> Result<(), BrokerError> {
    let chunk_count = match backend.get_slot(reference.as_str())? {
        Some(mut primary) => {
            let parsed = parse_chunk_manifest(&primary);
            primary.fill(0);
            parsed.ok().flatten()
        }
        None => return Ok(()),
    };
    if let Some(count) = chunk_count {
        let mut first_error = None;
        for index in 0..count {
            if let Err(error) = backend.delete_slot(&chunk_key(reference, index)) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
    }
    backend.delete_slot(reference.as_str())
}

impl OsCredentialStore {
    pub fn new(service: impl Into<String>) -> Result<Self, BrokerError> {
        let service = service.into();
        if service.trim().is_empty() {
            return Err(BrokerError::InvalidRequest(
                "credential service cannot be empty".to_owned(),
            ));
        }
        Ok(Self { service })
    }

    fn entry_for_key(&self, key: &str) -> Result<keyring::Entry, BrokerError> {
        keyring::Entry::new(&self.service, key).map_err(|_| BrokerError::CredentialStore)
    }
}

#[cfg(target_os = "windows")]
impl SecretSlotBackend for OsCredentialStore {
    fn set_slot(&self, key: &str, value: &[u8]) -> Result<(), BrokerError> {
        self.entry_for_key(key)?
            .set_secret(value)
            .map_err(|_| BrokerError::CredentialStore)
    }

    fn get_slot(&self, key: &str) -> Result<Option<Vec<u8>>, BrokerError> {
        match self.entry_for_key(key)?.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(BrokerError::CredentialStore),
        }
    }

    fn delete_slot(&self, key: &str) -> Result<(), BrokerError> {
        match self.entry_for_key(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(BrokerError::CredentialStore),
        }
    }
}

impl CredentialStore for OsCredentialStore {
    fn store(&self, secret: &SecretValue) -> Result<CredentialRef, BrokerError> {
        #[cfg(target_os = "windows")]
        {
            store_chunked(self, secret, WINDOWS_MAX_CHUNK_BYTES)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let reference = CredentialRef::new();
            self.entry_for_key(reference.as_str())?
                .set_secret(secret.expose())
                .map_err(|_| BrokerError::CredentialStore)?;
            Ok(reference)
        }
    }

    fn resolve(&self, reference: &CredentialRef) -> Result<Option<SecretValue>, BrokerError> {
        #[cfg(target_os = "windows")]
        {
            resolve_chunked(self, reference)
        }
        #[cfg(not(target_os = "windows"))]
        {
            match self.entry_for_key(reference.as_str())?.get_secret() {
                Ok(secret) => Ok(Some(SecretValue::new(secret)?)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(BrokerError::CredentialStore),
            }
        }
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), BrokerError> {
        #[cfg(target_os = "windows")]
        {
            delete_chunked(self, reference)
        }
        #[cfg(not(target_os = "windows"))]
        {
            match self.entry_for_key(reference.as_str())?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(BrokerError::CredentialStore),
            }
        }
    }
}

#[derive(Default)]
pub struct MemoryCredentialStore {
    values: Mutex<HashMap<CredentialRef, Vec<u8>>>,
}

impl CredentialStore for MemoryCredentialStore {
    fn store(&self, secret: &SecretValue) -> Result<CredentialRef, BrokerError> {
        let reference = CredentialRef::new();
        self.values
            .lock()
            .map_err(|_| BrokerError::CredentialStore)?
            .insert(reference.clone(), secret.expose().to_vec());
        Ok(reference)
    }

    fn resolve(&self, reference: &CredentialRef) -> Result<Option<SecretValue>, BrokerError> {
        self.values
            .lock()
            .map_err(|_| BrokerError::CredentialStore)?
            .get(reference)
            .map(|secret| SecretValue::new(secret.clone()))
            .transpose()
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), BrokerError> {
        if let Some(mut secret) = self
            .values
            .lock()
            .map_err(|_| BrokerError::CredentialStore)?
            .remove(reference)
        {
            secret.fill(0);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CappedSlotBackend {
        max_blob_bytes: usize,
        fail_on_write_index: Option<usize>,
        writes: Mutex<usize>,
        slots: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl CappedSlotBackend {
        fn new(max_blob_bytes: usize) -> Self {
            Self {
                max_blob_bytes,
                fail_on_write_index: None,
                writes: Mutex::new(0),
                slots: Mutex::new(HashMap::new()),
            }
        }

        fn failing_on_write(max_blob_bytes: usize, fail_on_write_index: usize) -> Self {
            Self {
                max_blob_bytes,
                fail_on_write_index: Some(fail_on_write_index),
                writes: Mutex::new(0),
                slots: Mutex::new(HashMap::new()),
            }
        }

        fn slot_count(&self) -> usize {
            self.slots.lock().unwrap().len()
        }
    }

    impl SecretSlotBackend for CappedSlotBackend {
        fn set_slot(&self, key: &str, value: &[u8]) -> Result<(), BrokerError> {
            let mut count = self.writes.lock().unwrap();
            let current = *count;
            *count += 1;
            if self.fail_on_write_index == Some(current) || value.len() > self.max_blob_bytes {
                return Err(BrokerError::CredentialStore);
            }
            self.slots
                .lock()
                .unwrap()
                .insert(key.to_owned(), value.to_vec());
            Ok(())
        }

        fn get_slot(&self, key: &str) -> Result<Option<Vec<u8>>, BrokerError> {
            Ok(self.slots.lock().unwrap().get(key).cloned())
        }

        fn delete_slot(&self, key: &str) -> Result<(), BrokerError> {
            if let Some(mut bytes) = self.slots.lock().unwrap().remove(key) {
                bytes.fill(0);
            }
            Ok(())
        }
    }

    #[test]
    fn chunked_storage_round_trips_and_deletes_secrets_exceeding_windows_blob_limit() {
        // Windows CRED_MAX_CREDENTIAL_BLOB_SIZE is 2560 bytes.
        let backend = CappedSlotBackend::new(2560);

        // Short secret fits in one slot without chunking.
        let short_ref = store_chunked(
            &backend,
            &SecretValue::new("short-token").unwrap(),
            WINDOWS_MAX_CHUNK_BYTES,
        )
        .unwrap();
        assert_eq!(backend.slot_count(), 1);
        assert_eq!(
            resolve_chunked(&backend, &short_ref)
                .unwrap()
                .unwrap()
                .expose(),
            b"short-token"
        );
        delete_chunked(&backend, &short_ref).unwrap();
        assert_eq!(backend.slot_count(), 0);
        assert!(resolve_chunked(&backend, &short_ref).unwrap().is_none());

        // Large 6000-byte and 32768-byte PATs exceed 2560 bytes and must chunk cleanly.
        for size in [6000_usize, 32768_usize] {
            let payload: Vec<u8> = (0..size).map(|i| b'A' + (i % 26) as u8).collect();
            let reference = store_chunked(
                &backend,
                &SecretValue::new(payload.clone()).unwrap(),
                WINDOWS_MAX_CHUNK_BYTES,
            )
            .unwrap();
            let expected_chunks = size.div_ceil(WINDOWS_MAX_CHUNK_BYTES);
            assert_eq!(backend.slot_count(), expected_chunks + 1);
            let resolved = resolve_chunked(&backend, &reference).unwrap().unwrap();
            assert_eq!(resolved.expose(), payload.as_slice());

            delete_chunked(&backend, &reference).unwrap();
            assert_eq!(backend.slot_count(), 0);
            assert!(resolve_chunked(&backend, &reference).unwrap().is_none());
        }
    }

    #[test]
    fn chunked_storage_protects_manifest_prefix_and_rolls_back_partial_writes() {
        let backend = CappedSlotBackend::new(2560);
        let mut tricky = CHUNK_MANIFEST_PREFIX.to_vec();
        tricky.extend_from_slice(b"99-literal-secret");
        let reference = store_chunked(
            &backend,
            &SecretValue::new(tricky.clone()).unwrap(),
            WINDOWS_MAX_CHUNK_BYTES,
        )
        .unwrap();
        assert_eq!(
            resolve_chunked(&backend, &reference)
                .unwrap()
                .unwrap()
                .expose(),
            tricky.as_slice()
        );
        delete_chunked(&backend, &reference).unwrap();
        assert_eq!(backend.slot_count(), 0);

        // Failing during chunk 2 (index 1) or manifest write (index 3) leaves zero orphaned slots.
        let large = SecretValue::new(vec![b'Z'; 6000]).unwrap();
        for fail_at in [1_usize, 3_usize] {
            let failing = CappedSlotBackend::failing_on_write(2560, fail_at);
            assert!(matches!(
                store_chunked(&failing, &large, WINDOWS_MAX_CHUNK_BYTES),
                Err(BrokerError::CredentialStore)
            ));
            assert_eq!(failing.slot_count(), 0, "failed write {fail_at} left slots behind");
        }

        // Deleting one chunk out-of-band causes resolve to fail rather than returning truncated data.
        let reference = store_chunked(&backend, &large, WINDOWS_MAX_CHUNK_BYTES).unwrap();
        backend.delete_slot(&chunk_key(&reference, 1)).unwrap();
        assert!(matches!(
            resolve_chunked(&backend, &reference),
            Err(BrokerError::CredentialStore)
        ));
        delete_chunked(&backend, &reference).unwrap();
        assert_eq!(backend.slot_count(), 0);
    }
}
