//! Fleet-wide, tenant-agnostic provider API keys & config overrides.
//!
//! Stores encrypted-at-rest overrides in the `fleet_provider_secrets` table.
//! Decryption uses the `MasterKey` derived from `FLEET_SECRETS_KEY`. If the
//! master key is unset (or `FLEET_SECRETS_KEY` is missing), `load_all` returns
//! an empty map — the service does not refuse to start.

use crate::fleet_secrets::{
    decrypt_api_key, encrypt_api_key, EncryptedPayload, FleetSecretsError, MasterKey,
    ProviderOverride,
};
use ares_types::types::{AppError, Result};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use tracing::warn;

/// One persisted row in `fleet_provider_secrets`, in its encrypted form.
/// `ciphertext` and `nonce` are `None` when the row only overrides
/// `api_base` / `default_model`.
#[derive(Debug, Clone)]
pub struct StoredProviderOverride {
    pub provider_name: String,
    pub ciphertext: Option<Vec<u8>>,
    pub nonce: Option<Vec<u8>>,
    pub api_base: Option<String>,
    pub default_model: Option<String>,
    pub fallback_providers: Vec<String>,
    pub has_api_key: bool,
    pub updated_at: i64,
    pub updated_by: String,
}

/// CRUD for `fleet_provider_secrets`. Pure data access — no encryption here.
pub struct FleetProviderSecretsStore<'a> {
    pool: &'a PgPool,
}

impl<'a> FleetProviderSecretsStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Load every row from the table, decrypted.
    ///
    /// On master-key error (missing env var, wrong key, malformed row) the
    /// offending row is skipped and a warning is logged; the rest of the map
    /// is still returned. This is intentional: a single corrupted row should
    /// not prevent the rest of the fleet from operating.
    pub async fn load_all(
        &self,
        master: Option<&MasterKey>,
    ) -> Result<HashMap<String, ProviderOverride>> {
        let rows = sqlx::query(
            "SELECT provider_name, ciphertext, nonce, api_base, default_model, \
                    fallback_providers, has_api_key, updated_at, updated_by \
             FROM fleet_provider_secrets",
        )
        .fetch_all(self.pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

        let mut out = HashMap::with_capacity(rows.len());
        for row in rows {
            let stored = row_to_stored(&row)?;
            let mut entry = ProviderOverride {
                api_key: None,
                api_base: stored.api_base.clone(),
                default_model: stored.default_model.clone(),
                fallback_providers: stored.fallback_providers.clone(),
                updated_at: stored.updated_at,
                updated_by: stored.updated_by.clone(),
                ..Default::default()
            };
            if stored.has_api_key {
                let Some(master) = master else {
                    warn!(
                        provider = %stored.provider_name,
                        "FLEET_SECRETS_KEY is unset; cannot decrypt stored key. Skipping."
                    );
                    continue;
                };
                let (Some(ciphertext), Some(nonce)) = (stored.ciphertext, stored.nonce) else {
                    warn!(
                        provider = %stored.provider_name,
                        "Row has has_api_key=true but missing ciphertext/nonce; skipping."
                    );
                    continue;
                };
                let payload = EncryptedPayload { nonce, ciphertext };
                match decrypt_api_key(&payload, master) {
                    Ok(plain) => entry.api_key = Some(plain),
                    Err(e) => {
                        warn!(
                            provider = %stored.provider_name,
                            error = %e,
                            "Failed to decrypt stored API key (probably FLEET_SECRETS_KEY \
                             changed since the row was written). Row is unreadable until \
                             the original key is restored. Skipping."
                        );
                        continue;
                    }
                }
            }
            out.insert(stored.provider_name, entry);
        }
        Ok(out)
    }

    /// Insert or update one row. `master` must be `Some` when `api_key` is
    /// being written; otherwise this returns `FleetSecretsError::MasterKeyUnset`.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert(
        &self,
        provider_name: &str,
        api_key: Option<&str>,
        api_base: Option<&str>,
        default_model: Option<&str>,
        fallback_providers: Option<&[String]>,
        master: Option<&MasterKey>,
        updated_by: &str,
    ) -> Result<StoredProviderOverride> {
        if provider_name.is_empty() {
            return Err(AppError::InvalidInput(
                "provider_name must not be empty".into(),
            ));
        }
        // Validate: at least one field must be set.
        if api_key.is_none() && api_base.is_none() && default_model.is_none() && fallback_providers.is_none() {
            return Err(AppError::InvalidInput(
                "At least one of api_key, api_base, default_model, fallback_providers must be provided".into(),
            ));
        }
        let (ciphertext, nonce, has_api_key) = match api_key {
            Some(plain) => {
                let master = master
                    .as_ref()
                    .ok_or(AppError::Configuration(FleetSecretsError::MasterKeyUnset.to_string()))?;
                let payload = encrypt_api_key(plain, master).map_err(|e| {
                    AppError::Configuration(format!("encrypt_api_key failed: {e}"))
                })?;
                (Some(payload.ciphertext), Some(payload.nonce), true)
            }
            None => (None, None, false),
        };

        // If neither the incoming api_key nor the existing row has one, but the
        // user is updating only api_base/default_model, we keep the existing
        // ciphertext. Look it up first to preserve it on a partial update.
        let existing = if !has_api_key {
            self.fetch_stored(provider_name).await?
        } else {
            None
        };

        let final_ciphertext: Option<Vec<u8>>;
        let final_nonce: Option<Vec<u8>>;
        let final_has_api_key: bool;
        if has_api_key {
            final_ciphertext = ciphertext;
            final_nonce = nonce;
            final_has_api_key = true;
        } else if let Some(prev) = existing {
            final_ciphertext = prev.ciphertext;
            final_nonce = prev.nonce;
            final_has_api_key = prev.has_api_key;
        } else {
            final_ciphertext = None;
            final_nonce = None;
            final_has_api_key = false;
        }

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let fallback_json = fallback_providers.and_then(|v| serde_json::to_value(v).ok());

        sqlx::query(
            "INSERT INTO fleet_provider_secrets \
                (provider_name, ciphertext, nonce, api_base, default_model, fallback_providers, has_api_key, updated_at, updated_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, to_timestamp($8), $9) \
             ON CONFLICT (provider_name) DO UPDATE SET \
                ciphertext = EXCLUDED.ciphertext, \
                nonce = EXCLUDED.nonce, \
                api_base = EXCLUDED.api_base, \
                default_model = EXCLUDED.default_model, \
                fallback_providers = EXCLUDED.fallback_providers, \
                has_api_key = EXCLUDED.has_api_key, \
                updated_at = EXCLUDED.updated_at, \
                updated_by = EXCLUDED.updated_by",
        )
        .bind(provider_name)
        .bind(&final_ciphertext)
        .bind(&final_nonce)
        .bind(api_base)
        .bind(default_model)
        .bind(&fallback_json)
        .bind(final_has_api_key)
        .bind(now_secs as f64)
        .bind(updated_by)
        .execute(self.pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

        self.fetch_stored(provider_name)
            .await?
            .ok_or_else(|| AppError::Database("upsert succeeded but row not found".into()))
    }

    /// Hard delete a row. Returns the number of rows affected (0 if not found).
    pub async fn delete(&self, provider_name: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM fleet_provider_secrets WHERE provider_name = $1")
            .bind(provider_name)
            .execute(self.pool)
            .await
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(res.rows_affected())
    }

    /// Fetch one row in its stored (encrypted) form. Returns None if not found.
    pub async fn fetch_stored(
        &self,
        provider_name: &str,
    ) -> Result<Option<StoredProviderOverride>> {
        let row = sqlx::query(
            "SELECT provider_name, ciphertext, nonce, api_base, default_model, \
                    fallback_providers, has_api_key, updated_at, updated_by \
             FROM fleet_provider_secrets WHERE provider_name = $1",
        )
        .bind(provider_name)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

        row.map(|r| row_to_stored(&r)).transpose()
    }

    /// List every provider name and has_api_key bool, without decrypting.
    /// Used by the admin GET endpoint to display a row count and a "Set / Not
    /// set" badge per provider.
    pub async fn list_metadata(
        &self,
    ) -> Result<Vec<(String, bool, Option<String>, Option<String>, Vec<String>, i64, String)>> {
        let rows = sqlx::query(
            "SELECT provider_name, has_api_key, api_base, default_model, fallback_providers, updated_at, updated_by \
             FROM fleet_provider_secrets ORDER BY provider_name",
        )
        .fetch_all(self.pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let name: String = row.try_get("provider_name").map_err(sqlx_err)?;
            let has: bool = row.try_get("has_api_key").map_err(sqlx_err)?;
            let api_base: Option<String> = row.try_get("api_base").map_err(sqlx_err)?;
            let default_model: Option<String> = row.try_get("default_model").map_err(sqlx_err)?;
            let fallback_json: Option<serde_json::Value> = row.try_get("fallback_providers").map_err(sqlx_err)?;
            let fallback_providers: Vec<String> = fallback_json
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let updated_at: chrono::DateTime<chrono::Utc> =
                row.try_get("updated_at").map_err(sqlx_err)?;
            let updated_by: String = row.try_get("updated_by").map_err(sqlx_err)?;
            out.push((
                name,
                has,
                api_base,
                default_model,
                fallback_providers,
                updated_at.timestamp(),
                updated_by,
            ));
        }
        Ok(out)
    }
}

fn row_to_stored(row: &sqlx::postgres::PgRow) -> Result<StoredProviderOverride> {
    let provider_name: String = row.try_get("provider_name").map_err(sqlx_err)?;
    let ciphertext: Option<Vec<u8>> = row.try_get("ciphertext").map_err(sqlx_err)?;
    let nonce: Option<Vec<u8>> = row.try_get("nonce").map_err(sqlx_err)?;
    let api_base: Option<String> = row.try_get("api_base").map_err(sqlx_err)?;
    let default_model: Option<String> = row.try_get("default_model").map_err(sqlx_err)?;
    let fallback_json: Option<serde_json::Value> = row.try_get("fallback_providers").map_err(sqlx_err)?;
    let fallback_providers: Vec<String> = fallback_json
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let has_api_key: bool = row.try_get("has_api_key").map_err(sqlx_err)?;
    let updated_at: chrono::DateTime<chrono::Utc> = row.try_get("updated_at").map_err(sqlx_err)?;
    let updated_by: String = row.try_get("updated_by").map_err(sqlx_err)?;
    Ok(StoredProviderOverride {
        provider_name,
        ciphertext,
        nonce,
        api_base,
        default_model,
        fallback_providers,
        has_api_key,
        updated_at: updated_at.timestamp(),
        updated_by,
    })
}

fn sqlx_err(e: sqlx::Error) -> AppError {
    AppError::Database(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet_secrets::{last_n_visible, MasterKey};

    // These tests do not require a live DB; they exercise the in-memory map
    // contract. Integration tests against a real Postgres instance should
    // live in tests/ at the crate root.

    #[test]
    fn provider_override_carries_all_fields() {
        let entry = ProviderOverride {
            api_key: Some("nvapi-X".into()),
            api_base: Some("https://example.com/v1".into()),
            default_model: Some("meta/llama-3.3-70b-instruct".into()),
            updated_at: 1,
            updated_by: "admin".into(),
            ..Default::default()
        };
        assert_eq!(entry.api_key.as_deref(), Some("nvapi-X"));
        let truncated = last_n_visible(entry.api_key.as_deref().unwrap(), 4);
        // "nvapi-X" has 7 chars; last 4 = "pi-X"
        assert_eq!(truncated.as_deref(), Some("…pi-X"));
    }

    #[test]
    fn master_key_from_env_handles_missing() {
        let prev = std::env::var("FLEET_SECRETS_KEY").ok();
        std::env::remove_var("FLEET_SECRETS_KEY");
        assert!(MasterKey::from_env().is_none());
        if let Some(p) = prev {
            std::env::set_var("FLEET_SECRETS_KEY", p);
        }
    }
}
