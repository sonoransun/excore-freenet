//! API key authentication provider.
//!
//! API keys are pre-shared secrets that map to a named principal with assigned
//! roles. Keys are stored as SHA-256 hashes so the raw secret is never
//! persisted.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::authz::Role;
use crate::config::GlobalRng;

use super::provider::{AuthError, AuthProvider, AuthResult};
use super::{ClientIdentity, Credential};

/// An API key with associated metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// Unique identifier for this key (not the secret itself).
    pub key_id: String,
    /// Human-readable name for this key.
    pub name: String,
    /// SHA-256 hash of the secret key material.
    pub key_hash: [u8; 32],
    /// Roles assigned to this key.
    pub roles: Vec<Role>,
    /// Whether this key is currently active.
    pub enabled: bool,
}

impl ApiKey {
    /// Create a new API key, returning the key metadata and the raw secret.
    ///
    /// The raw secret should be shown to the user exactly once; only the hash
    /// is stored.
    pub fn generate(name: String, roles: Vec<Role>) -> (Self, String) {
        let mut secret_bytes = [0u8; 32];
        GlobalRng::fill_bytes(&mut secret_bytes);
        let secret = bs58::encode(&secret_bytes).into_string();
        let key_hash = blake3::hash(secret.as_bytes()).into();

        let mut id_bytes = [0u8; 8];
        GlobalRng::fill_bytes(&mut id_bytes);
        let key_id = bs58::encode(&id_bytes).into_string();

        let key = Self {
            key_id,
            name,
            key_hash,
            roles,
            enabled: true,
        };
        (key, secret)
    }

    /// Verify that a raw secret matches this key's hash.
    #[cfg(test)]
    fn verify(&self, secret: &str) -> bool {
        let hash: [u8; 32] = blake3::hash(secret.as_bytes()).into();
        constant_time_eq(&hash, &self.key_hash)
    }
}

/// Constant-time byte comparison.
#[cfg(test)]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// Thread-safe in-memory store for API keys.
///
/// In production this would be backed by persistent storage; for now it
/// supports runtime registration and lookup.
#[derive(Debug, Clone)]
pub struct ApiKeyStore {
    /// Maps key_hash (hex-encoded) -> ApiKey for O(1) lookup during auth.
    keys: Arc<RwLock<HashMap<[u8; 32], ApiKey>>>,
}

impl Default for ApiKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiKeyStore {
    pub fn new() -> Self {
        Self {
            keys: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new API key. Returns the raw secret (show once to user).
    pub fn create_key(&self, name: String, roles: Vec<Role>) -> (ApiKey, String) {
        let (key, secret) = ApiKey::generate(name, roles);
        self.keys.write().insert(key.key_hash, key.clone());
        (key, secret)
    }

    /// Look up a key by verifying the raw secret.
    pub fn verify(&self, secret: &str) -> Option<ApiKey> {
        let hash: [u8; 32] = blake3::hash(secret.as_bytes()).into();
        let keys = self.keys.read();
        keys.get(&hash).filter(|k| k.enabled).cloned()
    }

    /// Revoke (disable) a key by its key_id.
    pub fn revoke(&self, key_id: &str) -> bool {
        let mut keys = self.keys.write();
        for key in keys.values_mut() {
            if key.key_id == key_id {
                key.enabled = false;
                return true;
            }
        }
        false
    }

    /// List all registered keys (without secret material).
    pub fn list_keys(&self) -> Vec<ApiKey> {
        self.keys.read().values().cloned().collect()
    }
}

/// Authentication provider that validates API keys against an `ApiKeyStore`.
pub struct ApiKeyProvider {
    store: ApiKeyStore,
}

impl ApiKeyProvider {
    pub fn new(store: ApiKeyStore) -> Self {
        Self { store }
    }
}

impl AuthProvider for ApiKeyProvider {
    fn supports(&self, credential: &Credential) -> bool {
        matches!(credential, Credential::ApiKey(_))
    }

    fn authenticate(&self, credential: &Credential) -> AuthResult {
        match credential {
            Credential::ApiKey(secret) => match self.store.verify(secret) {
                Some(key) => Ok(ClientIdentity::ApiKey {
                    key_id: key.key_id,
                    name: key.name,
                }),
                None => Err(AuthError::InvalidCredentials {
                    reason: "invalid API key".into(),
                }),
            },
            Credential::LegacyToken(_)
            | Credential::BearerToken(_)
            | Credential::None => Err(AuthError::NoCredentials),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::Role;

    #[test]
    fn test_api_key_generate_and_verify() {
        let (key, secret) = ApiKey::generate("test-key".into(), vec![Role::Admin]);
        assert!(key.verify(&secret));
        assert!(!key.verify("wrong-secret"));
    }

    #[test]
    fn test_api_key_store_create_and_verify() {
        let store = ApiKeyStore::new();
        let (key, secret) = store.create_key("my-key".into(), vec![Role::User]);

        let found = store.verify(&secret);
        assert!(found.is_some());
        assert_eq!(found.unwrap().key_id, key.key_id);

        assert!(store.verify("bogus").is_none());
    }

    #[test]
    fn test_api_key_store_revoke() {
        let store = ApiKeyStore::new();
        let (key, secret) = store.create_key("revokable".into(), vec![Role::User]);

        assert!(store.verify(&secret).is_some());
        assert!(store.revoke(&key.key_id));
        assert!(store.verify(&secret).is_none());
    }

    #[test]
    fn test_api_key_provider_authenticate() {
        let store = ApiKeyStore::new();
        let (_key, secret) = store.create_key("provider-test".into(), vec![Role::Admin]);
        let provider = ApiKeyProvider::new(store);

        let result = provider.authenticate(&Credential::ApiKey(secret));
        assert!(result.is_ok());

        let result = provider.authenticate(&Credential::ApiKey("bad".into()));
        assert!(result.is_err());

        // Non-API-key credential returns NoCredentials
        let result = provider.authenticate(&Credential::None);
        assert!(matches!(result, Err(AuthError::NoCredentials)));
    }

    #[test]
    fn test_constant_time_eq() {
        let a = [1u8, 2, 3, 4];
        let b = [1u8, 2, 3, 4];
        let c = [1u8, 2, 3, 5];
        assert!(constant_time_eq(&a, &b));
        assert!(!constant_time_eq(&a, &c));
        assert!(!constant_time_eq(&a, &[1, 2, 3]));
    }
}
