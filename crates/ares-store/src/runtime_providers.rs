//! Runtime-defined LLM provider database operations.
//!
//! Provides CRUD for `runtime_providers` table (migration 021).
//!
//! # Design Notes
//!
//! - `RuntimeProviderStore` holds a `&PgPool` (same lifetime pattern as
//!   `FleetProviderSecretsStore` and `RuntimeToolStore`).
//! - JSONB columns bind directly to `serde_json::Value` via sqlx.
//! - Timestamps stored as `BIGINT` (Unix epoch seconds), matching the rest of
//!   the `ares-store` crate.
//! - All UUID columns are stored/transferred as `String`.

use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

// =============================================================================
// Structs
// =============================================================================

/// One persisted row in `runtime_providers`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeProvider {
    pub id: String,
    pub tenant_id: Option<String>,
    pub name: String,
    pub display_name: String,
    pub provider_type: String,
    pub api_base: String,
    pub auth_type: String,
    pub default_model: Option<String>,
    pub headers: Option<serde_json::Value>,
    pub request_transform: Option<serde_json::Value>,
    pub response_transform: Option<serde_json::Value>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Request body for creating or updating a runtime provider.
#[derive(Debug, Deserialize)]
pub struct CreateRuntimeProviderRequest {
    pub tenant_id: Option<String>,
    pub name: String,
    pub display_name: String,
    pub provider_type: String,
    pub api_base: String,
    pub auth_type: String,
    pub default_model: Option<String>,
    pub headers: Option<serde_json::Value>,
    pub request_transform: Option<serde_json::Value>,
    pub response_transform: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

// =============================================================================
// Store
// =============================================================================

/// CRUD for `runtime_providers`.
pub struct RuntimeProviderStore<'a> {
    pool: &'a PgPool,
}

impl<'a> RuntimeProviderStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// List all runtime providers, optionally filtered by tenant.
    pub async fn list(&self, tenant_id: Option<&str>) -> Result<Vec<RuntimeProvider>> {
        let rows = if let Some(tid) = tenant_id {
            sqlx::query(
                r#"
                SELECT id, tenant_id, name, display_name, provider_type, api_base,
                       auth_type, default_model, headers, request_transform,
                       response_transform, enabled, created_at, updated_at
                FROM runtime_providers
                WHERE tenant_id = $1
                ORDER BY name, tenant_id NULLS FIRST
                "#,
            )
            .bind(tid)
            .fetch_all(self.pool)
            .await
        } else {
            sqlx::query(
                r#"
                SELECT id, tenant_id, name, display_name, provider_type, api_base,
                       auth_type, default_model, headers, request_transform,
                       response_transform, enabled, created_at, updated_at
                FROM runtime_providers
                WHERE tenant_id IS NULL
                ORDER BY name, tenant_id NULLS FIRST
                "#,
            )
            .fetch_all(self.pool)
            .await
        };

        rows.map_err(sqlx_err)?
            .iter()
            .map(row_to_runtime_provider)
            .collect()
    }

    /// List every runtime provider, including tenant-scoped rows.
    pub async fn list_all(&self) -> Result<Vec<RuntimeProvider>> {
        let rows = sqlx::query(
            r#"
            SELECT id, tenant_id, name, display_name, provider_type, api_base,
                   auth_type, default_model, headers, request_transform,
                   response_transform, enabled, created_at, updated_at
            FROM runtime_providers
            ORDER BY name, tenant_id NULLS FIRST
            "#,
        )
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        rows.iter().map(row_to_runtime_provider).collect()
    }

    /// Get a single fleet-wide runtime provider by name.
    pub async fn get(&self, name: &str) -> Result<Option<RuntimeProvider>> {
        self.get_scoped(None, name).await
    }

    /// Get a single runtime provider by its tenant scope and name.
    pub async fn get_scoped(
        &self,
        tenant_id: Option<&str>,
        name: &str,
    ) -> Result<Option<RuntimeProvider>> {
        let row = sqlx::query(
            r#"
            SELECT id, tenant_id, name, display_name, provider_type, api_base,
                   auth_type, default_model, headers, request_transform,
                   response_transform, enabled, created_at, updated_at
            FROM runtime_providers
            WHERE name = $2
              AND (($1::TEXT IS NULL AND tenant_id IS NULL) OR tenant_id = $1)
            "#,
        )
        .bind(tenant_id)
        .bind(name)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        match row {
            Some(r) => Ok(Some(row_to_runtime_provider(&r)?)),
            None => Ok(None),
        }
    }

    /// Upsert a runtime provider. Scope identity is `(tenant_id, name)`.
    pub async fn upsert(&self, req: &CreateRuntimeProviderRequest) -> Result<RuntimeProvider> {
        validate_provider_type(&req.provider_type)?;
        validate_auth_type(&req.auth_type)?;

        let now = chrono::Utc::now().timestamp();
        let enabled = req.enabled.unwrap_or(true);

        let row = sqlx::query(
            r#"
            WITH updated AS (
                UPDATE runtime_providers SET
                    display_name = $3,
                    provider_type = $4,
                    api_base = $5,
                    auth_type = $6,
                    default_model = $7,
                    headers = $8,
                    request_transform = $9,
                    response_transform = $10,
                    enabled = $11,
                    updated_at = $12
                WHERE name = $2
                  AND (($1::TEXT IS NULL AND tenant_id IS NULL) OR tenant_id = $1)
                RETURNING id, tenant_id, name, display_name, provider_type, api_base,
                          auth_type, default_model, headers, request_transform,
                          response_transform, enabled, created_at, updated_at
            ), inserted AS (
                INSERT INTO runtime_providers (
                    id, tenant_id, name, display_name, provider_type, api_base,
                    auth_type, default_model, headers, request_transform,
                    response_transform, enabled, created_at, updated_at
                )
                SELECT gen_random_uuid()::text, $1, $2, $3, $4, $5,
                       $6, $7, $8, $9, $10, $11, $12, $12
                WHERE NOT EXISTS (SELECT 1 FROM updated)
                RETURNING id, tenant_id, name, display_name, provider_type, api_base,
                          auth_type, default_model, headers, request_transform,
                          response_transform, enabled, created_at, updated_at
            )
            SELECT * FROM updated
            UNION ALL
            SELECT * FROM inserted
            "#,
        )
        .bind(&req.tenant_id)
        .bind(&req.name)
        .bind(&req.display_name)
        .bind(&req.provider_type)
        .bind(&req.api_base)
        .bind(&req.auth_type)
        .bind(&req.default_model)
        .bind(&req.headers)
        .bind(&req.request_transform)
        .bind(&req.response_transform)
        .bind(enabled)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_runtime_provider(&row)
    }

    /// Hard-delete a fleet-wide runtime provider by name. Returns the number of rows affected.
    pub async fn delete(&self, name: &str) -> Result<u64> {
        self.delete_scoped(None, name).await
    }

    /// Hard-delete a runtime provider by tenant scope and name.
    pub async fn delete_scoped(&self, tenant_id: Option<&str>, name: &str) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM runtime_providers WHERE name = $2 AND (($1::TEXT IS NULL AND tenant_id IS NULL) OR tenant_id = $1)",
        )
            .bind(tenant_id)
            .bind(name)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;

        Ok(result.rows_affected())
    }
}

// =============================================================================
// Row mappers
// =============================================================================

fn row_to_runtime_provider(row: &sqlx::postgres::PgRow) -> Result<RuntimeProvider> {
    Ok(RuntimeProvider {
        id: row.try_get("id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").ok(),
        name: row.try_get("name").map_err(sqlx_err)?,
        display_name: row.try_get("display_name").map_err(sqlx_err)?,
        provider_type: row.try_get("provider_type").map_err(sqlx_err)?,
        api_base: row.try_get("api_base").map_err(sqlx_err)?,
        auth_type: row.try_get("auth_type").map_err(sqlx_err)?,
        default_model: row.try_get("default_model").ok(),
        headers: row.try_get("headers").ok(),
        request_transform: row.try_get("request_transform").ok(),
        response_transform: row.try_get("response_transform").ok(),
        enabled: row.try_get("enabled").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
        updated_at: row.try_get("updated_at").map_err(sqlx_err)?,
    })
}

fn sqlx_err(e: sqlx::Error) -> AppError {
    AppError::Database(e.to_string())
}

fn validate_provider_type(t: &str) -> Result<()> {
    match t {
        "openai-compatible" | "anthropic-compatible" | "azure" | "azure-compatible"
        | "bedrock" | "bedrock-compatible" | "custom" => Ok(()),
        _ => Err(AppError::InvalidInput(format!(
            "Invalid provider_type '{}'. Must be: openai-compatible, anthropic-compatible, azure, azure-compatible, bedrock, bedrock-compatible, or custom",
            t
        ))),
    }
}

fn validate_auth_type(t: &str) -> Result<()> {
    match t {
        "api_key" | "oauth2" | "aws_sigv4" => Ok(()),
        _ => Err(AppError::InvalidInput(format!(
            "Invalid auth_type '{}'. Must be: api_key, oauth2, or aws_sigv4",
            t
        ))),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    async fn create_test_pool() -> PgPool {
        ares_test_support::pool().await
    }

    #[tokio::test]
    async fn test_runtime_provider_crud() {
        let pool = create_test_pool().await;
        let store = RuntimeProviderStore::new(&pool);

        // Clean up any leftover test row.
        let _ = store.delete("test_azure").await;

        let req = CreateRuntimeProviderRequest {
            tenant_id: None,
            name: "test_azure".to_string(),
            display_name: "Azure OpenAI".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://azure.openai.azure.com".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("gpt-4".to_string()),
            headers: Some(serde_json::json!({"api-version": "2024-02-01"})),
            request_transform: None,
            response_transform: None,
            enabled: Some(true),
        };

        let created = store.upsert(&req).await.expect("upsert should succeed");
        assert_eq!(created.name, "test_azure");
        assert_eq!(created.display_name, "Azure OpenAI");
        assert!(created.enabled);

        let fetched = store.get("test_azure").await.expect("get should succeed");
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.provider_type, "openai-compatible");

        let list = store.list(None).await.expect("list should succeed");
        assert!(list.iter().any(|p| p.name == "test_azure"));

        let deleted = store
            .delete("test_azure")
            .await
            .expect("delete should succeed");
        assert_eq!(deleted, 1);

        let after_delete = store
            .get("test_azure")
            .await
            .expect("get after delete should succeed");
        assert!(after_delete.is_none());
    }

    fn provider_req(
        name: &str,
        tenant_id: Option<&str>,
        display_name: &str,
        api_base: &str,
    ) -> CreateRuntimeProviderRequest {
        CreateRuntimeProviderRequest {
            tenant_id: tenant_id.map(str::to_string),
            name: name.to_string(),
            display_name: display_name.to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: api_base.to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("model".to_string()),
            headers: None,
            request_transform: None,
            response_transform: None,
            enabled: Some(true),
        }
    }

    async fn ensure_scoped_runtime_provider_index(pool: &PgPool) {
        sqlx::query("DROP INDEX IF EXISTS idx_runtime_providers_name")
            .execute(pool)
            .await
            .expect("drop old runtime provider index");
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_runtime_providers_scope_name ON runtime_providers (COALESCE(tenant_id, ''), name)",
        )
        .execute(pool)
        .await
        .expect("create scoped runtime provider index");
    }

    #[tokio::test]
    async fn scoped_identity_allows_global_and_tenant_same_name() {
        let pool = ares_test_support::pool().await;
        ensure_scoped_runtime_provider_index(&pool).await;
        let store = RuntimeProviderStore::new(&pool);
        let name = "test_shared_provider_scope";
        let _ = store.delete_scoped(None, name).await;
        let _ = store.delete_scoped(Some("tenant-a"), name).await;

        let global = provider_req(name, None, "Global Shared", "https://global.example.com");
        let tenant = provider_req(
            name,
            Some("tenant-a"),
            "Tenant Shared",
            "https://tenant.example.com",
        );

        let global_row = store.upsert(&global).await.expect("global upsert");
        let tenant_row = store.upsert(&tenant).await.expect("tenant upsert");
        assert_ne!(global_row.id, tenant_row.id);
        assert_eq!(global_row.tenant_id, None);
        assert_eq!(tenant_row.tenant_id.as_deref(), Some("tenant-a"));

        let global_fetched = store
            .get_scoped(None, name)
            .await
            .expect("global fetch")
            .expect("global row");
        let tenant_fetched = store
            .get_scoped(Some("tenant-a"), name)
            .await
            .expect("tenant fetch")
            .expect("tenant row");
        assert_eq!(global_fetched.display_name, "Global Shared");
        assert_eq!(tenant_fetched.display_name, "Tenant Shared");
        assert!(store
            .get_scoped(Some("tenant-b"), name)
            .await
            .expect("tenant-b fetch")
            .is_none());

        let updated_tenant = provider_req(
            name,
            Some("tenant-a"),
            "Tenant Updated",
            "https://tenant-updated.example.com",
        );
        store.upsert(&updated_tenant).await.expect("tenant update");
        let global_after = store
            .get_scoped(None, name)
            .await
            .expect("global refetch")
            .expect("global row after tenant update");
        let tenant_after = store
            .get_scoped(Some("tenant-a"), name)
            .await
            .expect("tenant refetch")
            .expect("tenant row after update");
        assert_eq!(global_after.api_base, "https://global.example.com");
        assert_eq!(tenant_after.display_name, "Tenant Updated");

        assert_eq!(
            store.delete_scoped(Some("tenant-a"), name).await.unwrap(),
            1
        );
        assert!(store
            .get_scoped(Some("tenant-a"), name)
            .await
            .unwrap()
            .is_none());
        assert!(store.get_scoped(None, name).await.unwrap().is_some());
        assert_eq!(store.delete_scoped(None, name).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn list_all_includes_global_and_tenant_scoped_providers() {
        let pool = create_test_pool().await;
        let store = RuntimeProviderStore::new(&pool);
        let _ = store.delete("test_global_provider").await;
        let _ = store.delete("test_tenant_provider").await;

        let global = provider_req(
            "test_global_provider",
            None,
            "Global Provider",
            "https://global.example.com",
        );
        let tenant = provider_req(
            "test_tenant_provider",
            Some("tenant-a"),
            "Tenant Provider",
            "https://tenant.example.com",
        );

        store.upsert(&global).await.expect("global upsert");
        store.upsert(&tenant).await.expect("tenant upsert");

        let global_only = store.list(None).await.expect("global list");
        assert!(global_only.iter().any(|p| p.name == "test_global_provider"));
        assert!(!global_only.iter().any(|p| p.name == "test_tenant_provider"));

        let all = store.list_all().await.expect("list all");
        assert!(all.iter().any(|p| p.name == "test_global_provider"));
        assert!(all.iter().any(|p| p.name == "test_tenant_provider"));

        let _ = store.delete("test_global_provider").await;
        let _ = store.delete("test_tenant_provider").await;
    }
}
