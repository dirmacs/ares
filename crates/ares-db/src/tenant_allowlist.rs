//! Per-tenant allowlist for tools, models, and RAG sources.
//!
//! Provides CRUD for `tenant_tool_allowlist`, `tenant_model_allowlist`,
//! and `tenant_rag_allowlist` tables (migration 022).

use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

// =============================================================================
// Structs
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenantToolAllowlistItem {
    pub id: String,
    pub tenant_id: String,
    pub tool_name: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenantModelAllowlistItem {
    pub id: String,
    pub tenant_id: String,
    pub model_id: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenantRagAllowlistItem {
    pub id: String,
    pub tenant_id: String,
    pub rag_source: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

// =============================================================================
// Store
// =============================================================================

pub struct TenantAllowlistStore<'a> {
    pool: &'a PgPool,
}

impl<'a> TenantAllowlistStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    // -------------------------------------------------------------------------
    // Tools
    // -------------------------------------------------------------------------

    pub async fn list_tools(&self, tenant_id: &str) -> Result<Vec<TenantToolAllowlistItem>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, tool_name, enabled, created_at, updated_at
             FROM tenant_tool_allowlist
             WHERE tenant_id = $1
             ORDER BY tool_name",
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(rows.iter().map(|r| row_to_tool(r)).collect())
    }

    pub async fn allow_tool(
        &self,
        tenant_id: &str,
        tool_name: &str,
    ) -> Result<TenantToolAllowlistItem> {
        let now = now_secs();

        let row = sqlx::query(
            "INSERT INTO tenant_tool_allowlist
                (id, tenant_id, tool_name, enabled, created_at, updated_at)
             VALUES (gen_random_uuid()::text, $1, $2, TRUE, $3, $3)
             ON CONFLICT (tenant_id, tool_name) DO UPDATE SET
                enabled = TRUE,
                updated_at = EXCLUDED.updated_at
             RETURNING id, tenant_id, tool_name, enabled, created_at, updated_at",
        )
        .bind(tenant_id)
        .bind(tool_name)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(row_to_tool(&row))
    }

    pub async fn deny_tool(&self, tenant_id: &str, tool_name: &str) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM tenant_tool_allowlist WHERE tenant_id = $1 AND tool_name = $2",
        )
        .bind(tenant_id)
        .bind(tool_name)
        .execute(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(result.rows_affected())
    }

    pub async fn is_tool_allowed(&self, tenant_id: &str, tool_name: &str) -> Result<bool> {
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tenant_tool_allowlist WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        if total == 0 {
            return Ok(true);
        }

        let enabled: Option<bool> = sqlx::query_scalar(
            "SELECT enabled FROM tenant_tool_allowlist WHERE tenant_id = $1 AND tool_name = $2",
        )
        .bind(tenant_id)
        .bind(tool_name)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(enabled.unwrap_or(false))
    }

    // -------------------------------------------------------------------------
    // Models
    // -------------------------------------------------------------------------

    pub async fn list_models(&self, tenant_id: &str) -> Result<Vec<TenantModelAllowlistItem>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, model_id, enabled, created_at, updated_at
             FROM tenant_model_allowlist
             WHERE tenant_id = $1
             ORDER BY model_id",
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(rows.iter().map(|r| row_to_model(r)).collect())
    }

    pub async fn allow_model(
        &self,
        tenant_id: &str,
        model_id: &str,
    ) -> Result<TenantModelAllowlistItem> {
        let now = now_secs();

        let row = sqlx::query(
            "INSERT INTO tenant_model_allowlist
                (id, tenant_id, model_id, enabled, created_at, updated_at)
             VALUES (gen_random_uuid()::text, $1, $2, TRUE, $3, $3)
             ON CONFLICT (tenant_id, model_id) DO UPDATE SET
                enabled = TRUE,
                updated_at = EXCLUDED.updated_at
             RETURNING id, tenant_id, model_id, enabled, created_at, updated_at",
        )
        .bind(tenant_id)
        .bind(model_id)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(row_to_model(&row))
    }

    pub async fn deny_model(&self, tenant_id: &str, model_id: &str) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM tenant_model_allowlist WHERE tenant_id = $1 AND model_id = $2",
        )
        .bind(tenant_id)
        .bind(model_id)
        .execute(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(result.rows_affected())
    }

    pub async fn is_model_allowed(&self, tenant_id: &str, model_id: &str) -> Result<bool> {
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tenant_model_allowlist WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        if total == 0 {
            return Ok(true);
        }

        let enabled: Option<bool> = sqlx::query_scalar(
            "SELECT enabled FROM tenant_model_allowlist WHERE tenant_id = $1 AND model_id = $2",
        )
        .bind(tenant_id)
        .bind(model_id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(enabled.unwrap_or(false))
    }

    // -------------------------------------------------------------------------
    // RAG Sources
    // -------------------------------------------------------------------------

    pub async fn list_rag_sources(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<TenantRagAllowlistItem>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, rag_source, enabled, created_at, updated_at
             FROM tenant_rag_allowlist
             WHERE tenant_id = $1
             ORDER BY rag_source",
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(rows.iter().map(|r| row_to_rag(r)).collect())
    }

    pub async fn allow_rag_source(
        &self,
        tenant_id: &str,
        rag_source: &str,
    ) -> Result<TenantRagAllowlistItem> {
        let now = now_secs();

        let row = sqlx::query(
            "INSERT INTO tenant_rag_allowlist
                (id, tenant_id, rag_source, enabled, created_at, updated_at)
             VALUES (gen_random_uuid()::text, $1, $2, TRUE, $3, $3)
             ON CONFLICT (tenant_id, rag_source) DO UPDATE SET
                enabled = TRUE,
                updated_at = EXCLUDED.updated_at
             RETURNING id, tenant_id, rag_source, enabled, created_at, updated_at",
        )
        .bind(tenant_id)
        .bind(rag_source)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(row_to_rag(&row))
    }

    pub async fn deny_rag_source(&self, tenant_id: &str, rag_source: &str) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM tenant_rag_allowlist WHERE tenant_id = $1 AND rag_source = $2",
        )
        .bind(tenant_id)
        .bind(rag_source)
        .execute(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(result.rows_affected())
    }

    pub async fn is_rag_source_allowed(
        &self,
        tenant_id: &str,
        rag_source: &str,
    ) -> Result<bool> {
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tenant_rag_allowlist WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        if total == 0 {
            return Ok(true);
        }

        let enabled: Option<bool> = sqlx::query_scalar(
            "SELECT enabled FROM tenant_rag_allowlist WHERE tenant_id = $1 AND rag_source = $2",
        )
        .bind(tenant_id)
        .bind(rag_source)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        Ok(enabled.unwrap_or(false))
    }
}

// =============================================================================
// Row mappers
// =============================================================================

fn row_to_tool(row: &sqlx::postgres::PgRow) -> TenantToolAllowlistItem {
    TenantToolAllowlistItem {
        id: row.try_get("id").unwrap_or_default(),
        tenant_id: row.try_get("tenant_id").unwrap_or_default(),
        tool_name: row.try_get("tool_name").unwrap_or_default(),
        enabled: row.try_get("enabled").unwrap_or_default(),
        created_at: row.try_get("created_at").unwrap_or_default(),
        updated_at: row.try_get("updated_at").unwrap_or_default(),
    }
}

fn row_to_model(row: &sqlx::postgres::PgRow) -> TenantModelAllowlistItem {
    TenantModelAllowlistItem {
        id: row.try_get("id").unwrap_or_default(),
        tenant_id: row.try_get("tenant_id").unwrap_or_default(),
        model_id: row.try_get("model_id").unwrap_or_default(),
        enabled: row.try_get("enabled").unwrap_or_default(),
        created_at: row.try_get("created_at").unwrap_or_default(),
        updated_at: row.try_get("updated_at").unwrap_or_default(),
    }
}

fn row_to_rag(row: &sqlx::postgres::PgRow) -> TenantRagAllowlistItem {
    TenantRagAllowlistItem {
        id: row.try_get("id").unwrap_or_default(),
        tenant_id: row.try_get("tenant_id").unwrap_or_default(),
        rag_source: row.try_get("rag_source").unwrap_or_default(),
        enabled: row.try_get("enabled").unwrap_or_default(),
        created_at: row.try_get("created_at").unwrap_or_default(),
        updated_at: row.try_get("updated_at").unwrap_or_default(),
    }
}

fn sqlx_err(e: sqlx::Error) -> AppError {
    AppError::Database(e.to_string())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_tool_allowlist_item_roundtrip() {
        let item = TenantToolAllowlistItem {
            id: "id-1".into(),
            tenant_id: "t-1".into(),
            tool_name: "web_search".into(),
            enabled: true,
            created_at: 1717690000,
            updated_at: 1717690000,
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: TenantToolAllowlistItem = serde_json::from_str(&json).unwrap();
        assert_eq!(item, back);
    }

    #[test]
    fn tenant_model_allowlist_item_roundtrip() {
        let item = TenantModelAllowlistItem {
            id: "id-2".into(),
            tenant_id: "t-1".into(),
            model_id: "gpt-4o".into(),
            enabled: true,
            created_at: 1717690000,
            updated_at: 1717690000,
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: TenantModelAllowlistItem = serde_json::from_str(&json).unwrap();
        assert_eq!(item, back);
    }

    #[test]
    fn tenant_rag_allowlist_item_roundtrip() {
        let item = TenantRagAllowlistItem {
            id: "id-3".into(),
            tenant_id: "t-1".into(),
            rag_source: "confluence".into(),
            enabled: true,
            created_at: 1717690000,
            updated_at: 1717690000,
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: TenantRagAllowlistItem = serde_json::from_str(&json).unwrap();
        assert_eq!(item, back);
    }
}
