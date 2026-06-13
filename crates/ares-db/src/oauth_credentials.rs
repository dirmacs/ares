//! Per-tenant OAuth credential database operations.
//!
//! Provides CRUD for `oauth_credentials` table (migration 023).
//! All sensitive fields (client_secret, access_token, refresh_token) are
//! encrypted at rest using AES-256-GCM via `ares_config::fleet_secrets`.
//! The store handles encryption/decryption internally; callers work with
//! plaintext strings in requests and `EncryptedPayload` structs in responses.

use ares_config::fleet_secrets::{
    encrypt_api_key, EncryptedPayload, MasterKey,
};
use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

// =============================================================================
// Structs
// =============================================================================

/// One persisted (and decrypted) row in `oauth_credentials`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredential {
    pub id: String,
    pub tenant_id: String,
    pub provider: String,
    pub connector_type: String,
    pub client_id: String,
    pub client_secret: EncryptedPayload,
    pub access_token: Option<EncryptedPayload>,
    pub refresh_token: Option<EncryptedPayload>,
    pub expires_at: Option<i64>,
    pub scope: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Request body for creating a new OAuth credential row.
#[derive(Debug, Deserialize)]
pub struct CreateOAuthCredentialRequest {
    pub tenant_id: String,
    pub provider: String,
    pub connector_type: String,
    pub client_id: String,
    pub client_secret: String,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    pub scope: Option<String>,
}

// =============================================================================
// Store
// =============================================================================

/// CRUD for `oauth_credentials`.
pub struct OAuthCredentialStore<'a> {
    pool: &'a PgPool,
}

impl<'a> OAuthCredentialStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Create a new OAuth credential row, encrypting sensitive fields.
    pub async fn create(&self, req: &CreateOAuthCredentialRequest) -> Result<OAuthCredential> {
        let master = MasterKey::from_env()
            .ok_or_else(|| AppError::Configuration("FLEET_SECRETS_KEY not set".into()))?;

        let client_secret = encrypt_api_key(&req.client_secret, &master)
            .map_err(|e| AppError::Configuration(format!("encrypt failed: {e}")))?;

        let access_token = req
            .access_token
            .as_ref()
            .map(|t| encrypt_api_key(t, &master))
            .transpose()
            .map_err(|e| AppError::Configuration(format!("encrypt failed: {e}")))?;

        let refresh_token = req
            .refresh_token
            .as_ref()
            .map(|t| encrypt_api_key(t, &master))
            .transpose()
            .map_err(|e| AppError::Configuration(format!("encrypt failed: {e}")))?;

        let now = chrono::Utc::now().timestamp();

        let row = sqlx::query(
            r#"
            INSERT INTO oauth_credentials (
                id, tenant_id, provider, connector_type, client_id,
                client_secret_ciphertext, client_secret_nonce,
                access_token_ciphertext, access_token_nonce,
                refresh_token_ciphertext, refresh_token_nonce,
                expires_at, scope, created_at, updated_at
            ) VALUES (
                gen_random_uuid()::text, $1, $2, $3, $4,
                $5, $6, $7, $8, $9, $10, $11, $12, $13, $13
            )
            RETURNING
                id, tenant_id, provider, connector_type, client_id,
                client_secret_ciphertext, client_secret_nonce,
                access_token_ciphertext, access_token_nonce,
                refresh_token_ciphertext, refresh_token_nonce,
                expires_at, scope, created_at, updated_at
            "#,
        )
        .bind(&req.tenant_id)
        .bind(&req.provider)
        .bind(&req.connector_type)
        .bind(&req.client_id)
        .bind(&client_secret.ciphertext)
        .bind(&client_secret.nonce)
        .bind(access_token.as_ref().map(|p| &p.ciphertext))
        .bind(access_token.as_ref().map(|p| &p.nonce))
        .bind(refresh_token.as_ref().map(|p| &p.ciphertext))
        .bind(refresh_token.as_ref().map(|p| &p.nonce))
        .bind(req.expires_at)
        .bind(&req.scope)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_oauth_credential(&row)
    }

    /// Get a single OAuth credential by tenant + provider + connector_type.
    /// Returns `None` if no row matches.
    pub async fn get(
        &self,
        tenant_id: &str,
        provider: &str,
        connector_type: &str,
    ) -> Result<Option<OAuthCredential>> {
        let row = sqlx::query(
            r#"
            SELECT
                id, tenant_id, provider, connector_type, client_id,
                client_secret_ciphertext, client_secret_nonce,
                access_token_ciphertext, access_token_nonce,
                refresh_token_ciphertext, refresh_token_nonce,
                expires_at, scope, created_at, updated_at
            FROM oauth_credentials
            WHERE tenant_id = $1 AND provider = $2 AND connector_type = $3
            "#,
        )
        .bind(tenant_id)
        .bind(provider)
        .bind(connector_type)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        match row {
            Some(r) => Ok(Some(row_to_oauth_credential(&r)?)),
            None => Ok(None),
        }
    }

    /// List all OAuth credentials for a given tenant.
    pub async fn list_by_tenant(&self, tenant_id: &str) -> Result<Vec<OAuthCredential>> {
        let rows = sqlx::query(
            r#"
            SELECT
                id, tenant_id, provider, connector_type, client_id,
                client_secret_ciphertext, client_secret_nonce,
                access_token_ciphertext, access_token_nonce,
                refresh_token_ciphertext, refresh_token_nonce,
                expires_at, scope, created_at, updated_at
            FROM oauth_credentials
            WHERE tenant_id = $1
            ORDER BY provider, connector_type
            "#,
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        rows.iter().map(row_to_oauth_credential).collect()
    }

    /// Update the access token, refresh token, and expiration for an existing row.
    pub async fn update_tokens(
        &self,
        id: &str,
        access_token: &str,
        refresh_token: Option<&str>,
        expires_at: i64,
    ) -> Result<()> {
        let master = MasterKey::from_env()
            .ok_or_else(|| AppError::Configuration("FLEET_SECRETS_KEY not set".into()))?;

        let at_payload = encrypt_api_key(access_token, &master)
            .map_err(|e| AppError::Configuration(format!("encrypt failed: {e}")))?;

        let rt_payload = refresh_token
            .map(|t| encrypt_api_key(t, &master))
            .transpose()
            .map_err(|e| AppError::Configuration(format!("encrypt failed: {e}")))?;

        let now = chrono::Utc::now().timestamp();

        sqlx::query(
            r#"
            UPDATE oauth_credentials
            SET
                access_token_ciphertext = $1,
                access_token_nonce = $2,
                refresh_token_ciphertext = $3,
                refresh_token_nonce = $4,
                expires_at = $5,
                updated_at = $6
            WHERE id = $7
            "#,
        )
        .bind(&at_payload.ciphertext)
        .bind(&at_payload.nonce)
        .bind(rt_payload.as_ref().map(|p| &p.ciphertext))
        .bind(rt_payload.as_ref().map(|p| &p.nonce))
        .bind(expires_at)
        .bind(now)
        .bind(id)
        .execute(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(())
    }

    /// Hard-delete a row by its id. Returns the number of rows affected.
    pub async fn delete(&self, id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM oauth_credentials WHERE id = $1")
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;

        Ok(res.rows_affected())
    }
}

// =============================================================================
// Row mappers
// =============================================================================

fn row_to_oauth_credential(row: &sqlx::postgres::PgRow) -> Result<OAuthCredential> {
    let id: String = row.try_get("id").map_err(sqlx_err)?;
    let tenant_id: String = row.try_get("tenant_id").map_err(sqlx_err)?;
    let provider: String = row.try_get("provider").map_err(sqlx_err)?;
    let connector_type: String = row.try_get("connector_type").map_err(sqlx_err)?;
    let client_id: String = row.try_get("client_id").map_err(sqlx_err)?;

    let client_secret_ciphertext: Vec<u8> = row.try_get("client_secret_ciphertext").map_err(sqlx_err)?;
    let client_secret_nonce: Vec<u8> = row.try_get("client_secret_nonce").map_err(sqlx_err)?;

    let access_token_ciphertext: Option<Vec<u8>> = row.try_get("access_token_ciphertext").map_err(sqlx_err)?;
    let access_token_nonce: Option<Vec<u8>> = row.try_get("access_token_nonce").map_err(sqlx_err)?;

    let refresh_token_ciphertext: Option<Vec<u8>> = row.try_get("refresh_token_ciphertext").map_err(sqlx_err)?;
    let refresh_token_nonce: Option<Vec<u8>> = row.try_get("refresh_token_nonce").map_err(sqlx_err)?;

    let expires_at: Option<i64> = row.try_get("expires_at").map_err(sqlx_err)?;
    let scope: Option<String> = row.try_get("scope").map_err(sqlx_err)?;
    let created_at: i64 = row.try_get("created_at").map_err(sqlx_err)?;
    let updated_at: i64 = row.try_get("updated_at").map_err(sqlx_err)?;

    let client_secret = EncryptedPayload {
        nonce: client_secret_nonce,
        ciphertext: client_secret_ciphertext,
    };

    let access_token = access_token_ciphertext.zip(access_token_nonce).map(|(ct, nonce)| EncryptedPayload {
        nonce,
        ciphertext: ct,
    });

    let refresh_token = refresh_token_ciphertext.zip(refresh_token_nonce).map(|(ct, nonce)| EncryptedPayload {
        nonce,
        ciphertext: ct,
    });

    Ok(OAuthCredential {
        id,
        tenant_id,
        provider,
        connector_type,
        client_id,
        client_secret,
        access_token,
        refresh_token,
        expires_at,
        scope,
        created_at,
        updated_at,
    })
}

fn sqlx_err(e: sqlx::Error) -> AppError {
    AppError::Database(e.to_string())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ares_config::fleet_secrets::decrypt_api_key;
    use sqlx::PgPool;

    async fn create_test_pool() -> PgPool {
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/ares".to_string());
        PgPool::connect(&database_url).await.unwrap()
    }

    fn ensure_master_key() {
        if std::env::var("FLEET_SECRETS_KEY").is_err() {
            std::env::set_var("FLEET_SECRETS_KEY", "test-master-key-for-oauth-credentials-12345");
        }
    }

    #[tokio::test]
    async fn test_oauth_credential_crud() {
        let pool = create_test_pool().await;
        let store = OAuthCredentialStore::new(&pool);
        ensure_master_key();

        let tenant_id = format!("test_tenant_{}", uuid::Uuid::new_v4());

        let req = CreateOAuthCredentialRequest {
            tenant_id: tenant_id.clone(),
            provider: "google".to_string(),
            connector_type: "oauth2".to_string(),
            client_id: "client-123".to_string(),
            client_secret: "super-secret".to_string(),
            access_token: Some("access-abc".to_string()),
            refresh_token: Some("refresh-xyz".to_string()),
            expires_at: Some(1_700_000_000),
            scope: Some("email profile".to_string()),
        };

        // Create
        let created = store.create(&req).await.expect("create should succeed");
        assert_eq!(created.tenant_id, tenant_id);
        assert_eq!(created.provider, "google");
        assert_eq!(created.connector_type, "oauth2");
        assert_eq!(created.client_id, "client-123");

        // Decrypt and verify the stored secret
        let master = MasterKey::from_env().unwrap();
        let decrypted_secret = decrypt_api_key(&created.client_secret, &master).unwrap();
        assert_eq!(decrypted_secret, "super-secret");

        // Get
        let fetched = store
            .get(&tenant_id, "google", "oauth2")
            .await
            .expect("get should succeed");
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.client_id, "client-123");

        // List by tenant
        let list = store
            .list_by_tenant(&tenant_id)
            .await
            .expect("list_by_tenant should succeed");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].provider, "google");

        // Update tokens
        store
            .update_tokens(&created.id, "new-access-token", Some("new-refresh-token"), 1_800_000_000)
            .await
            .expect("update_tokens should succeed");

        let after_update = store
            .get(&tenant_id, "google", "oauth2")
            .await
            .expect("get after update should succeed")
            .unwrap();
        let decrypted_at = decrypt_api_key(after_update.access_token.as_ref().unwrap(), &master).unwrap();
        let decrypted_rt = decrypt_api_key(after_update.refresh_token.as_ref().unwrap(), &master).unwrap();
        assert_eq!(decrypted_at, "new-access-token");
        assert_eq!(decrypted_rt, "new-refresh-token");
        assert_eq!(after_update.expires_at, Some(1_800_000_000));
        assert!(after_update.updated_at >= created.updated_at);

        // Delete
        let deleted = store.delete(&created.id).await.expect("delete should succeed");
        assert_eq!(deleted, 1);

        let after_delete = store
            .get(&tenant_id, "google", "oauth2")
            .await
            .expect("get after delete should succeed");
        assert!(after_delete.is_none());
    }

    #[tokio::test]
    async fn test_oauth_credential_without_optional_tokens() {
        let pool = create_test_pool().await;
        let store = OAuthCredentialStore::new(&pool);
        ensure_master_key();

        let tenant_id = format!("test_tenant_{}", uuid::Uuid::new_v4());

        let req = CreateOAuthCredentialRequest {
            tenant_id: tenant_id.clone(),
            provider: "github".to_string(),
            connector_type: "oauth_app".to_string(),
            client_id: "gh-client".to_string(),
            client_secret: "gh-secret".to_string(),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            scope: None,
        };

        let created = store.create(&req).await.expect("create should succeed");
        assert!(created.access_token.is_none());
        assert!(created.refresh_token.is_none());
        assert!(created.expires_at.is_none());
        assert!(created.scope.is_none());

        // Clean up
        let _ = store.delete(&created.id).await;
    }

    #[tokio::test]
    async fn test_oauth_credential_missing_master_key() {
        let pool = create_test_pool().await;
        let store = OAuthCredentialStore::new(&pool);

        // Temporarily remove the key
        let prev = std::env::var("FLEET_SECRETS_KEY").ok();
        std::env::remove_var("FLEET_SECRETS_KEY");

        let req = CreateOAuthCredentialRequest {
            tenant_id: "any".to_string(),
            provider: "google".to_string(),
            connector_type: "oauth2".to_string(),
            client_id: "c".to_string(),
            client_secret: "s".to_string(),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            scope: None,
        };

        let err = store.create(&req).await.unwrap_err();
        assert!(
            matches!(&err, AppError::Configuration(msg) if msg.contains("FLEET_SECRETS_KEY not set")),
            "expected Configuration error for missing master key, got: {err:?}"
        );

        // Restore
        if let Some(p) = prev {
            std::env::set_var("FLEET_SECRETS_KEY", p);
        }
    }
}
