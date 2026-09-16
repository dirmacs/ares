pub mod tenant;
pub use tenant::{
    normalize_api_key_scope, ApiKey, QuotaExceeded, Tenant, TenantContext, TenantQuota, TenantTier,
    API_KEY_MAX_TTL_DAYS, API_KEY_SCOPE_FULL, API_KEY_SCOPE_INGEST,
};

#[cfg(test)]
mod tests {
    use super::TenantTier;

    #[test]
    fn tenant_tier_reexport_is_usable() {
        let tier = TenantTier::Free;
        assert!(matches!(tier, TenantTier::Free));
    }
}
