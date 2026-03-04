use crate::db::TursoClient;
use crate::models::{ApiKey, Tenant, TenantContext, TenantQuota, TenantTier};
use crate::types::{AppError, Result};
use chrono::{TimeZone, Utc};
use libsql::params;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;

pub struct TenantDb {
    turso: Arc<TursoClient>,
    monthly_cache: Arc<RwLock<HashMap<String, (i64, u64)>>>,
    daily_cache: Arc<RwLock<HashMap<String, (i64, u64)>>>,
}

impl TenantDb {
    pub fn new(turso: Arc<TursoClient>) -> Self {
        Self {
            turso,
            monthly_cache: Arc::new(RwLock::new(HashMap::new())),
            daily_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn create_tenant(&self, name: String, tier: TenantTier) -> Result<Tenant> {
        let id = uuid::Uuid::new_v4().to_string();
        let tenant = Tenant::new(id.clone(), name, tier);

        let conn = self.turso.operation_conn().await?;
        conn.execute(
            "INSERT INTO tenants (id, name, tier, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
            params![
                &tenant.id,
                &tenant.name,
                tenant.tier.as_str(),
                tenant.created_at,
                tenant.updated_at
            ],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create tenant: {}", e)))?;

        Ok(tenant)
    }

    pub async fn list_tenants(&self) -> Result<Vec<Tenant>> {
        let conn = self.turso.operation_conn().await?;
        let mut rows = conn
            .query("SELECT id, name, tier, created_at, updated_at FROM tenants ORDER BY created_at DESC", ())
            .await
            .map_err(|e| AppError::Database(format!("Failed to list tenants: {}", e)))?;

        let mut tenants = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| AppError::Database(e.to_string()))? {
            let tier_str: String = row.get(2)?;
            let tier = TenantTier::from_str(&tier_str).unwrap_or(TenantTier::Free);
            tenants.push(Tenant {
                id: row.get(0)?,
                name: row.get(1)?,
                tier,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            });
        }

        Ok(tenants)
    }

    pub async fn get_tenant(&self, tenant_id: &str) -> Result<Option<Tenant>> {
        let conn = self.turso.operation_conn().await?;
        let mut rows = conn
            .query(
                "SELECT id, name, tier, created_at, updated_at FROM tenants WHERE id = ?",
                params![tenant_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to get tenant: {}", e)))?;

        if let Some(row) = rows.next().await.map_err(|e| AppError::Database(e.to_string()))? {
            let tier_str: String = row.get(2)?;
            let tier = TenantTier::from_str(&tier_str).unwrap_or(TenantTier::Free);
            Ok(Some(Tenant {
                id: row.get(0)?,
                name: row.get(1)?,
                tier,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn create_api_key(
        &self,
        tenant_id: &str,
        name: String,
    ) -> Result<(ApiKey, String)> {
        let id = uuid::Uuid::new_v4().to_string();
        let raw_key = generate_api_key();
        let key_prefix = format!("ares_{}", &raw_key[..8]);

        let key_hash = hash_api_key(&raw_key);

        let api_key = ApiKey::new(id, tenant_id.to_string(), key_hash, key_prefix, name);

        let conn = self.turso.operation_conn().await?;
        conn.execute(
            "INSERT INTO api_keys (id, tenant_id, key_hash, key_prefix, name, is_active, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                &api_key.id,
                &api_key.tenant_id,
                &api_key.key_hash,
                &api_key.key_prefix,
                &api_key.name,
                api_key.is_active as i32,
                api_key.created_at,
                api_key.expires_at
            ],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to create API key: {}", e)))?;

        Ok((api_key, raw_key))
    }

    pub async fn list_api_keys(&self, tenant_id: &str) -> Result<Vec<ApiKey>> {
        let conn = self.turso.operation_conn().await?;
        let mut rows = conn
            .query(
                "SELECT id, tenant_id, key_hash, key_prefix, name, is_active, created_at, expires_at FROM api_keys WHERE tenant_id = ? ORDER BY created_at DESC",
                params![tenant_id],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to list API keys: {}", e)))?;

        let mut keys = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| AppError::Database(e.to_string()))? {
            let expires_at: Option<i64> = row.get(7)?;
            keys.push(ApiKey {
                id: row.get(0)?,
                tenant_id: row.get(1)?,
                key_hash: row.get(2)?,
                key_prefix: row.get(3)?,
                name: row.get(4)?,
                is_active: row.get::<_, i32>(5)? != 0,
                created_at: row.get(6)?,
                expires_at,
            });
        }

        Ok(keys)
    }

    pub async fn verify_api_key(&self, raw_key: &str) -> Result<Option<TenantContext>> {
        let conn = self.turso.operation_conn().await?;

        let key_prefix = format!("ares_{}", &raw_key[5..13]);
        let mut rows = conn
            .query(
                "SELECT ak.id, ak.tenant_id, ak.key_hash, ak.is_active, ak.expires_at, t.tier 
                 FROM api_keys ak 
                 JOIN tenants t ON ak.tenant_id = t.id 
                 WHERE ak.key_prefix = ?",
                params![key_prefix],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to lookup API key: {}", e)))?;

        if let Some(row) = rows.next().await.map_err(|e| AppError::Database(e.to_string()))? {
            let key_hash: String = row.get(2)?;
            let is_active: bool = row.get::<_, i32>(3)? != 0;
            let expires_at: Option<i64> = row.get(4)?;
            let tier_str: String = row.get(5)?;

            if !is_active {
                return Ok(None);
            }

            if let Some(exp) = expires_at {
                if Utc::now().timestamp() > exp {
                    return Ok(None);
                }
            }

            let input_hash = hash_api_key(raw_key);
            if input_hash != key_hash {
                return Ok(None);
            }

            let tenant_id: String = row.get(1)?;
            let tier = TenantTier::from_str(&tier_str).unwrap_or(TenantTier::Free);

            Ok(Some(TenantContext::new(tenant_id, tier)))
        } else {
            Ok(None)
        }
    }

    pub async fn get_monthly_requests(&self, tenant_id: &str) -> Result<u64> {
        let cache_key = tenant_id.to_string();
        let now = Utc::now();
        let month_start = now.date_naive().with_day(1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();

        {
            let cache = self.monthly_cache.read().await;
            if let Some((cached_month, count)) = cache.get(&cache_key) {
                if *cached_month == month_start {
                    return Ok(*count);
                }
            }
        }

        let conn = self.turso.operation_conn().await?;
        let mut rows = conn
            .query(
                "SELECT COALESCE(SUM(request_count), 0) FROM monthly_usage_cache WHERE tenant_id = ? AND usage_month >= ?",
                params![tenant_id, month_start],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to get monthly requests: {}", e)))?;

        let count: u64 = if let Some(row) = rows.next().await.map_err(|e| AppError::Database(e.to_string()))? {
            row.get(0)?
        } else {
            0
        };

        {
            let mut cache = self.monthly_cache.write().await;
            cache.insert(cache_key, (month_start, count));
        }

        Ok(count)
    }

    pub async fn get_daily_requests(&self, tenant_id: &str) -> Result<u64> {
        let cache_key = tenant_id.to_string();
        let today = Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();

        {
            let cache = self.daily_cache.read().await;
            if let Some((cached_day, count)) = cache.get(&cache_key) {
                if *cached_day == today {
                    return Ok(*count);
                }
            }
        }

        let conn = self.turso.operation_conn().await?;
        let mut rows = conn
            .query(
                "SELECT COALESCE(SUM(request_count), 0) FROM daily_rate_limits WHERE tenant_id = ? AND usage_date >= ?",
                params![tenant_id, today],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to get daily requests: {}", e)))?;

        let count: u64 = if let Some(row) = rows.next().await.map_err(|e| AppError::Database(e.to_string()))? {
            row.get(0)?
        } else {
            0
        };

        {
            let mut cache = self.daily_cache.write().await;
            cache.insert(cache_key, (today, count));
        }

        Ok(count)
    }

    pub async fn record_usage_event(&self, tenant_id: &str, requests: u64, tokens: u64) -> Result<()> {
        let now = Utc::now();
        let today = now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let month_start = now.date_naive().with_day(1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();

        let conn = self.turso.operation_conn().await?;

        conn.execute(
            "INSERT INTO usage_events (id, tenant_id, request_count, token_count, created_at) VALUES (?, ?, ?, ?, ?)",
            params![
                uuid::Uuid::new_v4().to_string(),
                tenant_id,
                requests,
                tokens,
                now.timestamp()
            ],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to record usage event: {}", e)))?;

        conn.execute(
            "INSERT INTO monthly_usage_cache (tenant_id, usage_month, request_count, token_count) VALUES (?, ?, ?, ?)
             ON CONFLICT(tenant_id, usage_month) DO UPDATE SET 
             request_count = request_count + ?, token_count = token_count + ?",
            params![
                tenant_id,
                month_start,
                requests,
                tokens,
                requests,
                tokens
            ],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to update monthly cache: {}", e)))?;

        conn.execute(
            "INSERT INTO daily_rate_limits (tenant_id, usage_date, request_count) VALUES (?, ?, ?)
             ON CONFLICT(tenant_id, usage_date) DO UPDATE SET 
             request_count = request_count + ?",
            params![
                tenant_id,
                today,
                requests,
                requests
            ],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to update daily limit: {}", e)))?;

        {
            let mut cache = self.monthly_cache.write().await;
            if let Some((month, count)) = cache.get_mut(tenant_id) {
                if *month == month_start {
                    *count += requests;
                }
            }
        }

        {
            let mut cache = self.daily_cache.write().await;
            if let Some((day, count)) = cache.get_mut(tenant_id) {
                if *day == today {
                    *count += requests;
                }
            }
        }

        Ok(())
    }

    pub async fn get_usage_summary(
        &self,
        tenant_id: &str,
    ) -> Result<UsageSummary> {
        let conn = self.turso.operation_conn().await?;

        let monthly_requests = self.get_monthly_requests(tenant_id).await?;
        let daily_requests = self.get_daily_requests(tenant_id).await?;

        let now = Utc::now();
        let month_start = now.date_naive().with_day(1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();

        let mut rows = conn
            .query(
                "SELECT COALESCE(SUM(token_count), 0) FROM monthly_usage_cache WHERE tenant_id = ? AND usage_month >= ?",
                params![tenant_id, month_start],
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to get monthly tokens: {}", e)))?;

        let monthly_tokens: u64 = if let Some(row) = rows.next().await.map_err(|e| AppError::Database(e.to_string()))? {
            row.get(0)?
        } else {
            0
        };

        Ok(UsageSummary {
            monthly_requests,
            monthly_tokens,
            daily_requests,
        })
    }

    pub async fn update_tenant_quota(&self, tenant_id: &str, tier: TenantTier) -> Result<()> {
        let conn = self.turso.operation_conn().await?;
        conn.execute(
            "UPDATE tenants SET tier = ?, updated_at = ? WHERE id = ?",
            params![tier.as_str(), Utc::now().timestamp(), tenant_id],
        )
        .await
        .map_err(|e| AppError::Database(format!("Failed to update tenant quota: {}", e)))?;

        Ok(())
    }
}

fn generate_api_key() -> String {
    let bytes: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
    hex::encode(bytes)
}

fn hash_api_key(raw_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_key.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageSummary {
    pub monthly_requests: u64,
    pub monthly_tokens: u64,
    pub daily_requests: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_api_key() {
        let key = generate_api_key();
        assert!(key.starts_with("ares_"));
        assert_eq!(key.len(), 69);
    }
}
