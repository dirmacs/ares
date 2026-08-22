//! Fleet-wide, tenant-agnostic provider API key & config storage.
//!
//! Stores encrypted-at-rest provider overrides in PostgreSQL (loaded by
//! `ares-store::fleet_provider_secrets`), decrypts into an in-memory
//! `Arc<ArcSwap<FleetSecrets>>` map for lock-free hot-swap reads.
//!
//! Encryption: AES-256-GCM (RustCrypto `aes-gcm`) with a 96-bit nonce from
//! `OsRng` per encryption. Master key is SHA-256 of `FLEET_SECRETS_KEY` env
//! var, wrapped in `Zeroizing` so it is zeroed on drop.
//!
//! If `FLEET_SECRETS_KEY` is unset the module logs a single warning and
//! treats all getters as returning `None` — the service does NOT refuse to
//! start (OSS deployments without UI never set the master key).

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use arc_swap::ArcSwap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tracing::warn;
use zeroize::Zeroizing;

/// Fixed-length error type for the fleet-secrets module.
#[derive(Debug, Error)]
pub enum FleetSecretsError {
    #[error("master key derivation failed: {0}")]
    MasterKey(String),
    #[error("encryption failed: {0}")]
    Encrypt(String),
    #[error("decryption failed: {0}")]
    Decrypt(String),
    #[error("ciphertext is malformed: {0}")]
    Malformed(String),
    #[error("master key is not configured; fleet-secrets lookups return None")]
    MasterKeyUnset,
}

/// In-memory decrypted view of a single provider override row.
///
/// `None` fields mean "no override" — falls back to env-var / config defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderOverride {
    /// Decrypted API key (raw bytes interpreted as UTF-8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Override `api_base` (e.g. swap OpenAI endpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// Override `default_model` (e.g. switch to a different model id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Requests per minute limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_rpm: Option<i32>,
    /// Tokens per minute limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_tpm: Option<i32>,
    /// Fallback provider names to try if this provider fails.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_providers: Vec<String>,
    /// Unix seconds; set by the loader when the row is hydrated.
    #[serde(default)]
    pub updated_at: i64,
    /// Admin identity that last wrote the row.
    #[serde(default)]
    pub updated_by: String,
}

/// Decrypted, in-memory fleet secrets state. Cheap to clone (it's an
/// `Arc<ArcSwap<...>>` under the hood), so callers can hand the wrapper
/// directly to handlers.
#[derive(Debug, Clone, Default)]
pub struct FleetSecrets {
    inner: Arc<ArcSwap<FleetSecretsInner>>,
}

#[derive(Debug, Default)]
struct FleetSecretsInner {
    providers: HashMap<String, ProviderOverride>,
}

impl FleetSecrets {
    /// Construct an empty FleetSecrets wrapper.
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct from a prebuilt map (used by the DB loader).
    pub fn from_providers(providers: HashMap<String, ProviderOverride>) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(FleetSecretsInner { providers })),
        }
    }

    /// Atomic swap to a new map. Reads continue to see the old map; new
    /// readers see the new map.
    pub fn store(&self, providers: HashMap<String, ProviderOverride>) {
        self.inner.store(Arc::new(FleetSecretsInner { providers }));
    }

    /// Look up an override entry by provider name.
    pub fn get(&self, provider_name: &str) -> Option<ProviderOverride> {
        self.inner.load().providers.get(provider_name).cloned()
    }

    /// Return all overrides (cloned).
    pub fn list(&self) -> Vec<(String, ProviderOverride)> {
        self.inner
            .load()
            .providers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Total entries.
    pub fn len(&self) -> usize {
        self.inner.load().providers.len()
    }

    /// True if no entries.
    pub fn is_empty(&self) -> bool {
        self.inner.load().providers.is_empty()
    }
}

impl cordis::Service for FleetSecrets {
    fn name(&self) -> &'static str {
        "fleet_secrets"
    }
    fn init(
        &self,
        _ctx: &std::sync::Arc<cordis::Context>,
    ) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

/// AES-256-GCM ciphertext with its 96-bit nonce. Stored side-by-side so the
/// loader does not have to track nonces separately.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedPayload {
    /// 96-bit nonce, unique per encryption.
    pub nonce: Vec<u8>,
    /// AEAD ciphertext (includes GCM auth tag at the end).
    pub ciphertext: Vec<u8>,
}

/// Master-key wrapper that zeroes itself on drop. Cheap to clone via Arc.
#[derive(Debug, Clone)]
pub struct MasterKey {
    /// SHA-256(env) → 32 bytes. Wrapped in `Zeroizing` to scrub on drop.
    bytes: Arc<Zeroizing<[u8; 32]>>,
}

impl MasterKey {
    /// Resolve the master key from `FLEET_SECRETS_KEY` env var. Returns
    /// `None` and logs a single warning if the env var is unset/empty.
    pub fn from_env() -> Option<Self> {
        match std::env::var("FLEET_SECRETS_KEY") {
            Ok(raw) if !raw.is_empty() => Some(Self::from_secret(&raw)),
            _ => {
                warn!(
                    "FLEET_SECRETS_KEY is not set; fleet provider secrets will be disabled. \
                     Set it to a >=32-char random string in /etc/dirmacs/fleet-secrets.env \
                     and reload ares.service to enable encrypted provider overrides."
                );
                None
            }
        }
    }

    /// Build a MasterKey from an arbitrary string (for tests).
    pub fn from_secret(secret: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(secret.as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&digest);
        Self {
            bytes: Arc::new(Zeroizing::new(bytes)),
        }
    }

    /// Borrow the raw 32-byte key. Do not copy or log.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

/// Encrypt a UTF-8 plaintext API key. Returns the ciphertext + nonce.
pub fn encrypt_api_key(plaintext: &str, master: &MasterKey) -> Result<EncryptedPayload, FleetSecretsError> {
    let cipher = Aes256Gcm::new_from_slice(master.as_bytes())
        .map_err(|e| FleetSecretsError::Encrypt(e.to_string()))?;

    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad: b"ares.fleet_secrets.v1",
            },
        )
        .map_err(|e| FleetSecretsError::Encrypt(e.to_string()))?;

    Ok(EncryptedPayload {
        nonce: nonce_bytes.to_vec(),
        ciphertext,
    })
}

/// Decrypt a previously-encrypted payload back to the plaintext API key.
pub fn decrypt_api_key(
    payload: &EncryptedPayload,
    master: &MasterKey,
) -> Result<String, FleetSecretsError> {
    if payload.nonce.len() != 12 {
        return Err(FleetSecretsError::Malformed(format!(
            "nonce must be 12 bytes, got {}",
            payload.nonce.len()
        )));
    }
    if payload.ciphertext.is_empty() {
        return Err(FleetSecretsError::Malformed("ciphertext is empty".into()));
    }
    let cipher = Aes256Gcm::new_from_slice(master.as_bytes())
        .map_err(|e| FleetSecretsError::Decrypt(e.to_string()))?;
    let nonce = Nonce::from_slice(&payload.nonce);

    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &payload.ciphertext,
                aad: b"ares.fleet_secrets.v1",
            },
        )
        .map_err(|e| FleetSecretsError::Decrypt(e.to_string()))?;

    String::from_utf8(plaintext).map_err(|e| FleetSecretsError::Decrypt(e.to_string()))
}

/// Return the last `n` chars of a key, prefixed with `…`, for safe display.
/// Returns `None` for empty input.
pub fn last_n_visible(key: &str, n: usize) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    let len = key.chars().count();
    if len <= n {
        return Some(key.to_string());
    }
    let start_byte = key
        .char_indices()
        .nth(len - n)
        .map(|(i, _)| i)
        .unwrap_or(0);
    Some(format!("…{}", &key[start_byte..]))
}

/// Hex-encode a 32-byte key for diagnostic output (test-only).
#[cfg(test)]
pub fn hex_key(master: &MasterKey) -> String {
    hex::encode(master.as_bytes().as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_encrypt_decrypt() {
        let master = MasterKey::from_secret("test-secret");
        let plaintext = "nvapi-abc123-XYZ";
        let payload = encrypt_api_key(plaintext, &master).expect("encrypt");
        assert_eq!(payload.nonce.len(), 12);
        assert!(!payload.ciphertext.is_empty());
        let decrypted = decrypt_api_key(&payload, &master).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails_decrypt() {
        let a = MasterKey::from_secret("key-a");
        let b = MasterKey::from_secret("key-b");
        let payload = encrypt_api_key("secret", &a).expect("encrypt");
        let result = decrypt_api_key(&payload, &b);
        assert!(result.is_err(), "wrong master key must fail to decrypt");
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let master = MasterKey::from_secret("key");
        let mut payload = encrypt_api_key("secret", &master).expect("encrypt");
        // Flip a byte in the middle of the ciphertext (away from the auth tag).
        let mid = payload.ciphertext.len() / 2;
        payload.ciphertext[mid] ^= 0xFF;
        let result = decrypt_api_key(&payload, &master);
        assert!(result.is_err(), "tampered ciphertext must fail to decrypt");
    }

    #[test]
    fn malformed_nonce_rejected() {
        let master = MasterKey::from_secret("key");
        let payload = EncryptedPayload {
            nonce: vec![0; 8], // wrong length
            ciphertext: vec![1, 2, 3],
        };
        let result = decrypt_api_key(&payload, &master);
        assert!(result.is_err());
    }

    #[test]
    fn empty_ciphertext_rejected() {
        let master = MasterKey::from_secret("key");
        let payload = EncryptedPayload {
            nonce: vec![0; 12],
            ciphertext: vec![],
        };
        let result = decrypt_api_key(&payload, &master);
        assert!(result.is_err());
    }

    #[test]
    fn unique_nonce_per_encryption() {
        let master = MasterKey::from_secret("key");
        let a = encrypt_api_key("same", &master).expect("a");
        let b = encrypt_api_key("same", &master).expect("b");
        assert_ne!(a.nonce, b.nonce, "nonces must be unique");
    }

    #[test]
    fn last_n_visible_truncates() {
        // "nvapi-abc12345XYZ" has 18 chars; last 4 = "5XYZ"
        assert_eq!(last_n_visible("nvapi-abc12345XYZ", 4), Some("…5XYZ".to_string()));
        // 9 chars, n=8: 9>8, return "…" + last 8 = "…vapi-abc"
        assert_eq!(last_n_visible("nvapi-abc", 8), Some("…vapi-abc".to_string()));
        // 9 chars, n=10: 9<=10, return full
        assert_eq!(last_n_visible("nvapi-abc", 10), Some("nvapi-abc".to_string()));
        // Empty: None.
        assert_eq!(last_n_visible("", 4), None);
    }

    #[test]
    fn fleet_secrets_swap_is_visible_to_readers() {
        let secrets = FleetSecrets::new();
        assert!(secrets.get("nvidia").is_none());
        assert!(secrets.is_empty());

        let mut map = HashMap::new();
        map.insert(
            "nvidia".to_string(),
            ProviderOverride {
                api_key: Some("nvapi-X".into()),
                api_base: None,
                default_model: Some("meta/llama-3.3-70b-instruct".into()),
                updated_at: 1,
                updated_by: "admin".into(),
                ..Default::default()
            },
        );
        secrets.store(map);

        let entry = secrets.get("nvidia").expect("entry present");
        assert_eq!(entry.api_key.as_deref(), Some("nvapi-X"));
        assert_eq!(secrets.len(), 1);

        // Replace with empty map.
        secrets.store(HashMap::new());
        assert!(secrets.get("nvidia").is_none());
        assert!(secrets.is_empty());
    }

    #[test]
    fn from_env_returns_none_when_unset() {
        // SAFETY: tests in this module are not run in parallel, so mutating
        // the env var is safe.
        let prev = std::env::var("FLEET_SECRETS_KEY").ok();
        std::env::remove_var("FLEET_SECRETS_KEY");
        assert!(MasterKey::from_env().is_none());
        if let Some(p) = prev {
            std::env::set_var("FLEET_SECRETS_KEY", p);
        }
    }

    #[test]
    fn from_env_resolves_when_set() {
        std::env::set_var("FLEET_SECRETS_KEY", "test-only-secret-not-real");
        let m = MasterKey::from_env().expect("key resolves");
        // Verify the master key produces consistent SHA-256.
        let expected = MasterKey::from_secret("test-only-secret-not-real");
        assert_eq!(m.as_bytes(), expected.as_bytes());
    }
}
