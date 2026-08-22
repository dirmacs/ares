pub mod types;
pub mod models;
pub use models::{ApiKey, QuotaExceeded, Tenant, TenantContext, TenantQuota, TenantTier};
pub use types::{AppError, ErrorCode, Result};

#[cfg(test)]
mod tests {
    use super::{ApiKey, AppError, ErrorCode, QuotaExceeded, Result, Tenant, TenantContext, TenantQuota, TenantTier};

    #[test]
    fn test_public_reexports_are_usable() {
        let tier = TenantTier::Free;
        let quota = TenantQuota::from_tier(&tier);
        let tenant = Tenant::new("id".into(), "name".into(), tier);
        let ctx = TenantContext::new(tenant.id.clone(), tier);
        let _key = ApiKey::new(
            "k".into(),
            tenant.id,
            "hash".into(),
            "prefix".into(),
            "key".into(),
        );
        let result: Result<()> = Err(AppError::NotFound("missing".into()));
        assert!(matches!(result.unwrap_err().code(), ErrorCode::NotFound));
        assert_eq!(ctx.quota.requests_per_month, quota.requests_per_month);
        assert_eq!(QuotaExceeded::Monthly.message(), "Monthly request quota exceeded");
    }
}
