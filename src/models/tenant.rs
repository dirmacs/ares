use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TenantTier {
    Free,
    Dev,
    Pro,
    Enterprise,
}

impl TenantTier {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "free" => Some(TenantTier::Free),
            "dev" => Some(TenantTier::Dev),
            "pro" => Some(TenantTier::Pro),
            "enterprise" => Some(TenantTier::Enterprise),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TenantTier::Free => "free",
            TenantTier::Dev => "dev",
            TenantTier::Pro => "pro",
            TenantTier::Enterprise => "enterprise",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantQuota {
    pub tier: TenantTier,
    pub requests_per_month: u64,
    pub tokens_per_month: u64,
    pub max_agents: u32,
    pub requests_per_day: u64,
}

impl Default for TenantQuota {
    fn default() -> Self {
        Self::free()
    }
}

impl TenantQuota {
    pub fn free() -> Self {
        Self {
            tier: TenantTier::Free,
            requests_per_month: 1_000,
            tokens_per_month: 100_000,
            max_agents: 1,
            requests_per_day: 50,
        }
    }

    pub fn dev() -> Self {
        Self {
            tier: TenantTier::Dev,
            requests_per_month: 50_000,
            tokens_per_month: 5_000_000,
            max_agents: 10,
            requests_per_day: 2_000,
        }
    }

    pub fn pro() -> Self {
        Self {
            tier: TenantTier::Pro,
            requests_per_month: 500_000,
            tokens_per_month: 50_000_000,
            max_agents: u32::MAX,
            requests_per_day: 20_000,
        }
    }

    pub fn enterprise() -> Self {
        Self {
            tier: TenantTier::Enterprise,
            requests_per_month: u64::MAX,
            tokens_per_month: u64::MAX,
            max_agents: u32::MAX,
            requests_per_day: u64::MAX,
        }
    }

    pub fn from_tier(tier: &TenantTier) -> Self {
        match tier {
            TenantTier::Free => Self::free(),
            TenantTier::Dev => Self::dev(),
            TenantTier::Pro => Self::pro(),
            TenantTier::Enterprise => Self::enterprise(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub id: String,
    pub name: String,
    pub tier: TenantTier,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Tenant {
    pub fn new(id: String, name: String, tier: TenantTier) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id,
            name,
            tier,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub tenant_id: String,
    pub key_hash: String,
    pub key_prefix: String,
    pub name: String,
    pub is_active: bool,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

impl ApiKey {
    pub fn new(
        id: String,
        tenant_id: String,
        key_hash: String,
        key_prefix: String,
        name: String,
    ) -> Self {
        Self {
            id,
            tenant_id,
            key_hash,
            key_prefix,
            name,
            is_active: true,
            created_at: chrono::Utc::now().timestamp(),
            expires_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantContext {
    pub tenant_id: String,
    pub tier: TenantTier,
    pub quota: TenantQuota,
}

impl TenantContext {
    pub fn new(tenant_id: String, tier: TenantTier) -> Self {
        Self {
            tenant_id,
            tier,
            quota: TenantQuota::from_tier(&tier),
        }
    }

    pub fn can_make_request(&self, monthly_requests: u64, daily_requests: u64) -> bool {
        if monthly_requests >= self.quota.requests_per_month {
            return false;
        }
        if daily_requests >= self.quota.requests_per_day {
            return false;
        }
        true
    }

    pub fn can_use_tokens(&self, monthly_tokens: u64, additional_tokens: u64) -> bool {
        let Some(new_total) = monthly_tokens.checked_add(additional_tokens) else {
            return false;
        };
        new_total <= self.quota.tokens_per_month
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_from_str() {
        assert_eq!(TenantTier::from_str("free"), Some(TenantTier::Free));
        assert_eq!(TenantTier::from_str("dev"), Some(TenantTier::Dev));
        assert_eq!(TenantTier::from_str("pro"), Some(TenantTier::Pro));
        assert_eq!(
            TenantTier::from_str("enterprise"),
            Some(TenantTier::Enterprise)
        );
        assert_eq!(TenantTier::from_str("unknown"), None);
    }

    #[test]
    fn test_tier_as_str() {
        assert_eq!(TenantTier::Free.as_str(), "free");
        assert_eq!(TenantTier::Dev.as_str(), "dev");
        assert_eq!(TenantTier::Pro.as_str(), "pro");
        assert_eq!(TenantTier::Enterprise.as_str(), "enterprise");
    }

    #[test]
    fn test_free_quota() {
        let quota = TenantQuota::free();
        assert_eq!(quota.tier, TenantTier::Free);
        assert_eq!(quota.requests_per_month, 1_000);
        assert_eq!(quota.tokens_per_month, 100_000);
        assert_eq!(quota.max_agents, 1);
        assert_eq!(quota.requests_per_day, 50);
    }

    #[test]
    fn test_dev_quota() {
        let quota = TenantQuota::dev();
        assert_eq!(quota.tier, TenantTier::Dev);
        assert_eq!(quota.requests_per_month, 50_000);
        assert_eq!(quota.tokens_per_month, 5_000_000);
        assert_eq!(quota.max_agents, 10);
        assert_eq!(quota.requests_per_day, 2_000);
    }

    #[test]
    fn test_pro_quota() {
        let quota = TenantQuota::pro();
        assert_eq!(quota.tier, TenantTier::Pro);
        assert_eq!(quota.requests_per_month, 500_000);
        assert_eq!(quota.tokens_per_month, 50_000_000);
        assert_eq!(quota.max_agents, u32::MAX);
        assert_eq!(quota.requests_per_day, 20_000);
    }

    #[test]
    fn test_enterprise_quota() {
        let quota = TenantQuota::enterprise();
        assert_eq!(quota.tier, TenantTier::Enterprise);
        assert_eq!(quota.requests_per_month, u64::MAX);
        assert_eq!(quota.tokens_per_month, u64::MAX);
    }

    #[test]
    fn test_quota_from_tier() {
        assert_eq!(
            TenantQuota::from_tier(&TenantTier::Free).requests_per_month,
            1_000
        );
        assert_eq!(
            TenantQuota::from_tier(&TenantTier::Dev).requests_per_month,
            50_000
        );
        assert_eq!(
            TenantQuota::from_tier(&TenantTier::Pro).requests_per_month,
            500_000
        );
    }

    #[test]
    fn test_tenant_context_can_make_request() {
        let ctx = TenantContext::new("test".to_string(), TenantTier::Free);
        assert!(ctx.can_make_request(0, 0));
        assert!(ctx.can_make_request(999, 0));
        assert!(ctx.can_make_request(0, 49));
        assert!(!ctx.can_make_request(1000, 0));
        assert!(!ctx.can_make_request(0, 50));
    }

    #[test]
    fn test_tenant_context_can_use_tokens() {
        let ctx = TenantContext::new("test".to_string(), TenantTier::Free);
        assert!(ctx.can_use_tokens(0, 100_000));
        assert!(ctx.can_use_tokens(50_000, 50_000));
        assert!(!ctx.can_use_tokens(50_000, 50_001));
        assert!(!ctx.can_use_tokens(100_000, 1));
    }

    #[test]
    fn test_tenant_creation() {
        let tenant = Tenant::new("t1".to_string(), "Test Tenant".to_string(), TenantTier::Dev);
        assert_eq!(tenant.id, "t1");
        assert_eq!(tenant.name, "Test Tenant");
        assert_eq!(tenant.tier, TenantTier::Dev);
        assert!(tenant.created_at > 0);
    }

    #[test]
    fn test_api_key_creation() {
        let key = ApiKey::new(
            "k1".to_string(),
            "t1".to_string(),
            "hash123".to_string(),
            "ares_abc".to_string(),
            "Test Key".to_string(),
        );
        assert_eq!(key.id, "k1");
        assert_eq!(key.tenant_id, "t1");
        assert!(key.is_active);
        assert!(key.created_at > 0);
    }
}
