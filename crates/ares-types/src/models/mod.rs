pub mod tenant;
pub use tenant::{
    API_KEY_MAX_TTL_DAYS, API_KEY_SCOPE_FULL, API_KEY_SCOPE_INGEST, ApiKey, QuotaExceeded, Tenant,
    TenantContext, TenantQuota, TenantTier, normalize_api_key_scope,
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
