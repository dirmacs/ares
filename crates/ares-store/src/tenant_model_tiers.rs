//! Per-tenant model tier mapping.
//!
//! Maps abstract tiers (e.g. `powerful`, `fast`, `cheap`) to concrete
//! `(provider, model)` pairs on a per-tenant basis. Falls back to the
//! global [`AresConfig`] `models` map when no tenant-specific override
//! exists.

use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenantModelTier {
    pub tenant_id: String,
    pub tier_name: String,
    pub provider_name: String,
    pub model_name: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct SetTenantModelTierRequest {
    pub provider_name: String,
    pub model_name: String,
}

pub struct TenantModelTierStore<'a> {
    pool: &'a PgPool,
}

impl<'a> TenantModelTierStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, tenant_id: &str, tier_name: &str) -> Result<Option<TenantModelTier>> {
        let row = sqlx::query(
            "SELECT tenant_id, tier_name, provider_name, model_name, created_at, updated_at
             FROM tenant_model_tiers
             WHERE tenant_id = $1 AND tier_name = $2",
        )
        .bind(tenant_id)
        .bind(tier_name)
        .fetch_optional(self.pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(row.map(|r| row_to_tier(&r)))
    }

    pub async fn list_for_tenant(&self, tenant_id: &str) -> Result<Vec<TenantModelTier>> {
        let rows = sqlx::query(
            "SELECT tenant_id, tier_name, provider_name, model_name, created_at, updated_at
             FROM tenant_model_tiers
             WHERE tenant_id = $1
             ORDER BY tier_name",
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(rows.into_iter().map(|r| row_to_tier(&r)).collect())
    }

    pub async fn set(
        &self,
        tenant_id: &str,
        tier_name: &str,
        req: &SetTenantModelTierRequest,
    ) -> Result<TenantModelTier> {
        if tenant_id.is_empty() {
            return Err(AppError::InvalidInput("tenant_id must not be empty".into()));
        }
        if tier_name.is_empty() {
            return Err(AppError::InvalidInput("tier_name must not be empty".into()));
        }
        if req.provider_name.is_empty() {
            return Err(AppError::InvalidInput(
                "provider_name must not be empty".into(),
            ));
        }
        if req.model_name.is_empty() {
            return Err(AppError::InvalidInput(
                "model_name must not be empty".into(),
            ));
        }

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        sqlx::query(
            "INSERT INTO tenant_model_tiers
                (tenant_id, tier_name, provider_name, model_name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (tenant_id, tier_name) DO UPDATE SET
                provider_name = EXCLUDED.provider_name,
                model_name = EXCLUDED.model_name,
                updated_at = EXCLUDED.updated_at",
        )
        .bind(tenant_id)
        .bind(tier_name)
        .bind(&req.provider_name)
        .bind(&req.model_name)
        .bind(now_secs)
        .bind(now_secs)
        .execute(self.pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

        self.get(tenant_id, tier_name)
            .await?
            .ok_or_else(|| AppError::Database("upsert succeeded but row not found".into()))
    }

    pub async fn delete(&self, tenant_id: &str, tier_name: &str) -> Result<u64> {
        let result =
            sqlx::query("DELETE FROM tenant_model_tiers WHERE tenant_id = $1 AND tier_name = $2")
                .bind(tenant_id)
                .bind(tier_name)
                .execute(self.pool)
                .await
                .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(result.rows_affected())
    }
}

fn row_to_tier(row: &sqlx::postgres::PgRow) -> TenantModelTier {
    TenantModelTier {
        tenant_id: row.try_get("tenant_id").unwrap_or_default(),
        tier_name: row.try_get("tier_name").unwrap_or_default(),
        provider_name: row.try_get("provider_name").unwrap_or_default(),
        model_name: row.try_get("model_name").unwrap_or_default(),
        created_at: row.try_get("created_at").unwrap_or_default(),
        updated_at: row.try_get("updated_at").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_model_tier_roundtrip() {
        let tier = TenantModelTier {
            tenant_id: "t-123".into(),
            tier_name: "powerful".into(),
            provider_name: "openai".into(),
            model_name: "gpt-4o".into(),
            created_at: 1717690000,
            updated_at: 1717690000,
        };
        let json = serde_json::to_string(&tier).unwrap();
        let back: TenantModelTier = serde_json::from_str(&json).unwrap();
        assert_eq!(tier, back);
    }
}
