pub mod tenant;
pub use tenant::{ApiKey, Tenant, TenantContext, TenantQuota, TenantTier};

#[cfg(test)]
mod tests {
    use super::TenantTier;

    #[test]
    fn tenant_tier_reexport_is_usable() {
        let tier = TenantTier::Free;
        assert!(matches!(tier, TenantTier::Free));
    }
}
