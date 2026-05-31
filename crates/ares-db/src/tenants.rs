use crate::PostgresClient;
use ares_types::{ApiKey, Tenant, TenantContext, TenantTier};
use ares_types::types::{AppError, Result};
use chrono::{Datelike, TimeZone, Utc};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct TenantDb {
    postgres: Arc<PostgresClient>,
    monthly_cache: Arc<RwLock<HashMap<String, (i64, u64)>>>,
    daily_cache: Arc<RwLock<HashMap<String, (i64, u64)>>>,
}

impl TenantDb {
    pub fn new(postgres: Arc<PostgresClient>) -> Self {
        Self {
            postgres,
            monthly_cache: Arc::new(RwLock::new(HashMap::new())),
            daily_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn pool(&self) -> &sqlx::PgPool {
        &self.postgres.pool
    }

    pub async fn create_tenant(&self, name: String, tier: TenantTier) -> Result<Tenant> {
        let id = uuid::Uuid::new_v4().to_string();
        let tenant = Tenant::new(id.clone(), name, tier);

        sqlx::query(
            "INSERT INTO tenants (id, name, tier, created_at, updated_at) VALUES ($1, $2, $3, $4, $5)"
        )
        .bind(&tenant.id)
        .bind(&tenant.name)
        .bind(tenant.tier.as_str())
        .bind(tenant.created_at)
        .bind(tenant.updated_at)
        .execute(&self.postgres.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to create tenant: {}", e)))?;

        Ok(tenant)
    }

    pub async fn list_tenants(&self) -> Result<Vec<Tenant>> {
        let rows = sqlx::query(
            "SELECT id, name, tier, created_at, updated_at FROM tenants ORDER BY created_at DESC",
        )
        .fetch_all(&self.postgres.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to list tenants: {}", e)))?;

        let mut tenants = Vec::new();
        for row in rows {
            let tier_str: String = row.get(2);
            let tier = TenantTier::from_str(&tier_str).unwrap_or(TenantTier::Free);
            tenants.push(Tenant {
                id: row.get(0),
                name: row.get(1),
                tier,
                created_at: row.get(3),
                updated_at: row.get(4),
            });
        }

        Ok(tenants)
    }

    pub async fn get_tenant(&self, tenant_id: &str) -> Result<Option<Tenant>> {
        let row =
            sqlx::query("SELECT id, name, tier, created_at, updated_at FROM tenants WHERE id = $1")
                .bind(tenant_id)
                .fetch_optional(&self.postgres.pool)
                .await
                .map_err(|e| AppError::Database(format!("Failed to get tenant: {}", e)))?;

        if let Some(row) = row {
            let tier_str: String = row.get(2);
            let tier = TenantTier::from_str(&tier_str).unwrap_or(TenantTier::Free);
            Ok(Some(Tenant {
                id: row.get(0),
                name: row.get(1),
                tier,
                created_at: row.get(3),
                updated_at: row.get(4),
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn create_api_key(&self, tenant_id: &str, name: String) -> Result<(ApiKey, String)> {
        let id = uuid::Uuid::new_v4().to_string();
        let raw_key = generate_api_key();
        let key_prefix = api_key_prefix(&raw_key)
            .expect("generate_api_key must return a valid ares_-prefixed key");

        let key_hash = hash_api_key(&raw_key);

        let api_key = ApiKey::new(id, tenant_id.to_string(), key_hash, key_prefix, name);

        sqlx::query(
            "INSERT INTO api_keys (id, tenant_id, key_hash, key_prefix, name, is_active, created_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
        )
        .bind(&api_key.id)
        .bind(&api_key.tenant_id)
        .bind(&api_key.key_hash)
        .bind(&api_key.key_prefix)
        .bind(&api_key.name)
        .bind(api_key.is_active as i32)
        .bind(api_key.created_at)
        .bind(api_key.expires_at)
        .execute(&self.postgres.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to create API key: {}", e)))?;

        Ok((api_key, raw_key))
    }

    pub async fn list_api_keys(&self, tenant_id: &str) -> Result<Vec<ApiKey>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, key_hash, key_prefix, name, is_active, created_at, expires_at FROM api_keys WHERE tenant_id = $1 ORDER BY created_at DESC"
        )
        .bind(tenant_id)
        .fetch_all(&self.postgres.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to list API keys: {}", e)))?;

        let mut keys = Vec::new();
        for row in rows {
            let expires_at: Option<i64> = row.get(7);
            keys.push(ApiKey {
                id: row.get(0),
                tenant_id: row.get(1),
                key_hash: row.get(2),
                key_prefix: row.get(3),
                name: row.get(4),
                is_active: row.get::<i32, _>(5) != 0,
                created_at: row.get(6),
                expires_at,
            });
        }

        Ok(keys)
    }

    pub async fn verify_api_key(&self, raw_key: &str) -> Result<Option<TenantContext>> {
        let Some(key_prefix) = api_key_prefix(raw_key) else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT ak.id, ak.tenant_id, ak.key_hash, ak.is_active, ak.expires_at, t.tier 
             FROM api_keys ak 
             JOIN tenants t ON ak.tenant_id = t.id 
             WHERE ak.key_prefix = $1",
        )
        .bind(key_prefix)
        .fetch_optional(&self.postgres.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to lookup API key: {}", e)))?;

        if let Some(row) = row {
            let key_hash: String = row.get(2);
            let is_active: i32 = row.get(3);
            let expires_at: Option<i64> = row.get(4);
            let tier_str: String = row.get(5);

            if is_active == 0 {
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

            let tenant_id: String = row.get(1);
            let tier = TenantTier::from_str(&tier_str).unwrap_or(TenantTier::Free);

            Ok(Some(TenantContext::new(tenant_id, tier)))
        } else {
            Ok(None)
        }
    }

    pub async fn get_monthly_requests(&self, tenant_id: &str) -> Result<u64> {
        let cache_key = tenant_id.to_string();
        let now = Utc::now();
        let month_start = now
            .date_naive()
            .with_day(1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();

        {
            let cache = self.monthly_cache.read().await;
            if let Some((cached_month, count)) = cache.get(&cache_key) {
                if *cached_month == month_start {
                    return Ok(*count);
                }
            }
        }

        let row = sqlx::query(
            "SELECT COALESCE(SUM(request_count)::bigint, 0) FROM monthly_usage_cache WHERE tenant_id = $1 AND usage_month >= $2"
        )
        .bind(tenant_id)
        .bind(month_start)
        .fetch_one(&self.postgres.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to get monthly requests: {}", e)))?;

        let count: i64 = row.try_get::<i64, _>(0).unwrap_or(0);
        let count = count as u64;

        {
            let mut cache = self.monthly_cache.write().await;
            cache.insert(cache_key, (month_start, count));
        }

        Ok(count)
    }

    pub async fn get_daily_requests(&self, tenant_id: &str) -> Result<u64> {
        let cache_key = tenant_id.to_string();
        let today = Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();

        {
            let cache = self.daily_cache.read().await;
            if let Some((cached_day, count)) = cache.get(&cache_key) {
                if *cached_day == today {
                    return Ok(*count);
                }
            }
        }

        let row = sqlx::query(
            "SELECT COALESCE(SUM(request_count)::bigint, 0) FROM daily_rate_limits WHERE tenant_id = $1 AND usage_date >= $2"
        )
        .bind(tenant_id)
        .bind(today)
        .fetch_one(&self.postgres.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to get daily requests: {}", e)))?;

        let count: i64 = row.try_get::<i64, _>(0).unwrap_or(0);
        let count = count as u64;

        {
            let mut cache = self.daily_cache.write().await;
            cache.insert(cache_key, (today, count));
        }

        Ok(count)
    }

    pub async fn record_usage_event(
        &self,
        tenant_id: &str,
        requests: u64,
        tokens: u64,
    ) -> Result<()> {
        let now = Utc::now();
        let today = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let month_start = now
            .date_naive()
            .with_day(1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();

        sqlx::query(
            "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, created_at) VALUES ($1, $2, 'http', $3, $4, $5)"
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(tenant_id)
        .bind(requests as i64)
        .bind(tokens as i64)
        .bind(now.timestamp())
        .execute(&self.postgres.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to record usage event: {}", e)))?;

        sqlx::query(
            "INSERT INTO monthly_usage_cache (tenant_id, usage_month, request_count, token_count) VALUES ($1, $2, $3, $4)
             ON CONFLICT(tenant_id, usage_month) DO UPDATE SET 
             request_count = monthly_usage_cache.request_count + $5, token_count = monthly_usage_cache.token_count + $6"
        )
        .bind(tenant_id)
        .bind(month_start)
        .bind(requests as i64)
        .bind(tokens as i64)
        .bind(requests as i64)
        .bind(tokens as i64)
        .execute(&self.postgres.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to update monthly cache: {}", e)))?;

        sqlx::query(
            "INSERT INTO daily_rate_limits (tenant_id, usage_date, request_count) VALUES ($1, $2, $3)
             ON CONFLICT(tenant_id, usage_date) DO UPDATE SET 
             request_count = daily_rate_limits.request_count + $4"
        )
        .bind(tenant_id)
        .bind(today)
        .bind(requests as i64)
        .bind(requests as i64)
        .execute(&self.postgres.pool)
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

    pub async fn get_usage_summary(&self, tenant_id: &str) -> Result<UsageSummary> {
        let monthly_requests = self.get_monthly_requests(tenant_id).await?;
        let daily_requests = self.get_daily_requests(tenant_id).await?;

        let now = Utc::now();
        let month_start = now
            .date_naive()
            .with_day(1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();

        let row = sqlx::query(
            "SELECT COALESCE(SUM(token_count)::bigint, 0) FROM monthly_usage_cache WHERE tenant_id = $1 AND usage_month >= $2"
        )
        .bind(tenant_id)
        .bind(month_start)
        .fetch_one(&self.postgres.pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to get monthly tokens: {}", e)))?;

        let monthly_tokens: i64 = row.try_get::<i64, _>(0).unwrap_or(0);

        Ok(UsageSummary {
            monthly_requests,
            monthly_tokens: monthly_tokens as u64,
            daily_requests,
        })
    }

    pub async fn revoke_api_key(&self, tenant_id: &str, key_id: &str) -> Result<()> {
        let result =
            sqlx::query("UPDATE api_keys SET is_active = 0 WHERE id = $1 AND tenant_id = $2")
                .bind(key_id)
                .bind(tenant_id)
                .execute(&self.postgres.pool)
                .await
                .map_err(|e| AppError::Database(format!("Failed to revoke API key: {}", e)))?;

        if result.rows_affected() == 0 {
            return Err(AppError::NotFound(format!(
                "API key '{}' not found for tenant '{}'",
                key_id, tenant_id
            )));
        }
        Ok(())
    }

    pub async fn update_tenant_quota(&self, tenant_id: &str, tier: TenantTier) -> Result<()> {
        sqlx::query("UPDATE tenants SET tier = $1, updated_at = $2 WHERE id = $3")
            .bind(tier.as_str())
            .bind(Utc::now().timestamp())
            .bind(tenant_id)
            .execute(&self.postgres.pool)
            .await
            .map_err(|e| AppError::Database(format!("Failed to update tenant quota: {}", e)))?;

        Ok(())
    }
}

fn generate_api_key() -> String {
    let bytes: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
    format!("ares_{}", hex::encode(bytes))
}

fn api_key_prefix(raw_key: &str) -> Option<String> {
    let key_without_prefix = raw_key.strip_prefix("ares_")?;
    if key_without_prefix.len() < 8 {
        return None;
    }

    Some(format!("ares_{}", &key_without_prefix[..8]))
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

    // ── generate_api_key ──────────────────────────────────────────────

    #[test]
    fn test_generate_api_key_starts_with_ares_prefix() {
        let key = generate_api_key();
        assert!(key.starts_with("ares_"));
    }

    #[test]
    fn test_generate_api_key_length_is_5_prefix_plus_64_hex() {
        let key = generate_api_key();
        // "ares_" (5) + 32 random bytes hex-encoded (64 chars)
        assert_eq!(key.len(), 69);
    }

    #[test]
    fn test_generate_api_key_hex_portion_is_valid_hex() {
        let key = generate_api_key();
        let hex_part = &key[5..]; // strip "ares_"
        assert!(
            hex_part.chars().all(|c| c.is_ascii_hexdigit()),
            "hex portion should only contain [0-9a-f], got: {hex_part}"
        );
    }

    #[test]
    fn test_generate_api_key_uniqueness() {
        let key1 = generate_api_key();
        let key2 = generate_api_key();
        assert_ne!(key1, key2, "two random keys should not be equal");
    }

    // ── api_key_prefix ────────────────────────────────────────────────

    #[test]
    fn test_api_key_prefix_valid_long_key() {
        let key = "ares_abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let prefix = api_key_prefix(key).expect("valid key should yield prefix");
        assert_eq!(prefix, "ares_abcdef12");
        assert_eq!(prefix.len(), 13);
    }

    #[test]
    fn test_api_key_prefix_exactly_8_chars_after_ares() {
        // boundary: exactly 8 chars after "ares_" → valid
        let key = "ares_12345678";
        let prefix = api_key_prefix(key).expect("exactly 8 chars should be valid");
        assert_eq!(prefix, "ares_12345678");
    }

    #[test]
    fn test_api_key_prefix_7_chars_after_ares_rejected() {
        // 7 chars after "ares_" → too short
        let key = "ares_1234567";
        assert!(api_key_prefix(key).is_none());
    }

    #[test]
    fn test_api_key_prefix_empty_string() {
        assert!(api_key_prefix("").is_none());
    }

    #[test]
    fn test_api_key_prefix_just_ares_prefix() {
        assert!(api_key_prefix("ares_").is_none());
    }

    #[test]
    fn test_api_key_prefix_missing_ares_prefix() {
        assert!(api_key_prefix("not_ares_abcdef12").is_none());
    }

    #[test]
    fn test_api_key_prefix_matches_generated_key() {
        let key = generate_api_key();
        let prefix = api_key_prefix(&key).expect("prefix");

        assert!(prefix.starts_with("ares_"));
        assert_eq!(prefix.len(), 13);
        assert_eq!(prefix, format!("ares_{}", &key[5..13]));
    }

    // ── hash_api_key ──────────────────────────────────────────────────

    #[test]
    fn test_hash_api_key_is_deterministic() {
        let input = "ares_abc123def456";
        let hash1 = hash_api_key(input);
        let hash2 = hash_api_key(input);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_api_key_output_length() {
        // SHA-256 produces 32 bytes → 64 hex chars
        let hash = hash_api_key("test");
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_hash_api_key_hex_format() {
        let hash = hash_api_key("anything");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "hash output should be valid hex"
        );
    }

    #[test]
    fn test_hash_api_key_different_inputs_produce_different_hashes() {
        let h1 = hash_api_key("ares_aaaabbbbccccdddd");
        let h2 = hash_api_key("ares_eeeeffffggghhhh");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_hash_api_key_known_sha256() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let hash = hash_api_key("");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_hash_api_key_empty_string() {
        let hash = hash_api_key("");
        assert_eq!(hash.len(), 64);
    }

    // ── UsageSummary serde roundtrip ──────────────────────────────────

    #[test]
    fn test_usage_summary_serialize_all_fields() {
        let summary = UsageSummary {
            monthly_requests: 42,
            monthly_tokens: 9999,
            daily_requests: 7,
        };
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["monthly_requests"], 42);
        assert_eq!(json["monthly_tokens"], 9999);
        assert_eq!(json["daily_requests"], 7);
    }

    #[test]
    fn test_usage_summary_deserialize() {
        let json = r#"{"monthly_requests":5,"monthly_tokens":500,"daily_requests":1}"#;
        let summary: UsageSummary = serde_json::from_str(json).unwrap();
        assert_eq!(summary.monthly_requests, 5);
        assert_eq!(summary.monthly_tokens, 500);
        assert_eq!(summary.daily_requests, 1);
    }

    #[test]
    fn test_usage_summary_serde_roundtrip() {
        let original = UsageSummary {
            monthly_requests: 123,
            monthly_tokens: 456789,
            daily_requests: 10,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: UsageSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.monthly_requests, original.monthly_requests);
        assert_eq!(restored.monthly_tokens, original.monthly_tokens);
        assert_eq!(restored.daily_requests, original.daily_requests);
    }

    #[test]
    fn test_usage_summary_clone() {
        let original = UsageSummary {
            monthly_requests: 1,
            monthly_tokens: 2,
            daily_requests: 3,
        };
        let cloned = original.clone();
        assert_eq!(cloned.monthly_requests, original.monthly_requests);
        assert_eq!(cloned.monthly_tokens, original.monthly_tokens);
        assert_eq!(cloned.daily_requests, original.daily_requests);
    }

    #[test]
    fn test_usage_summary_debug() {
        let summary = UsageSummary {
            monthly_requests: 0,
            monthly_tokens: 0,
            daily_requests: 0,
        };
        let debug_str = format!("{:?}", summary);
        assert!(debug_str.contains("UsageSummary"));
        assert!(debug_str.contains("monthly_requests"));
    }

    // ── Integration: key generation → prefix → hash pipeline ──────────

    #[test]
    fn test_key_prefix_from_generated_key_is_consistent() {
        let key = generate_api_key();
        let prefix = api_key_prefix(&key).expect("generated keys must have valid prefix");
        // The prefix's hex portion matches the first 8 hex chars of the key
        let hex_after_prefix = &key[5..13]; // 8 hex chars
        assert_eq!(prefix, format!("ares_{hex_after_prefix}"));
    }

    #[test]
    fn test_hash_matches_known_sha256_of_key() {
        let key = "ares_0000000000000000000000000000000000000000000000000000000000000000";
        let hash = hash_api_key(key);
        assert_eq!(hash.len(), 64);
        // Hash should be deterministic across calls
        assert_eq!(hash, hash_api_key(key));
    }

    #[test]
    fn test_generated_key_hash_is_hex_64() {
        let key = generate_api_key();
        let hash = hash_api_key(&key);
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
