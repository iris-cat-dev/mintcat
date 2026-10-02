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

    fn entry(&self, reference: &CredentialRef) -> Result<keyring::Entry, BrokerError> {
        keyring::Entry::new(&self.service, reference.as_str())
            .map_err(|_| BrokerError::CredentialStore)
    }
}

impl CredentialStore for OsCredentialStore {
    fn store(&self, secret: &SecretValue) -> Result<CredentialRef, BrokerError> {
        let reference = CredentialRef::new();
        self.entry(&reference)?
            .set_secret(secret.expose())
            .map_err(|_| BrokerError::CredentialStore)?;
        Ok(reference)
    }

    fn resolve(&self, reference: &CredentialRef) -> Result<Option<SecretValue>, BrokerError> {
        match self.entry(reference)?.get_secret() {
            Ok(secret) => Ok(Some(SecretValue::new(secret)?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(BrokerError::CredentialStore),
        }
    }

    fn delete(&self, reference: &CredentialRef) -> Result<(), BrokerError> {
        match self.entry(reference)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(BrokerError::CredentialStore),
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
