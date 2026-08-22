// ares/src/mcp/auth.rs
// Extracts and validates API key from MCP connection configuration.
// The API key is passed as an environment variable when the MCP server process is spawned.

#[cfg(feature = "postgres")]
use ares_types::TenantContext;

/// Error type for MCP authentication.
#[derive(Debug, thiserror::Error)]
pub enum McpAuthError {
    #[error("No API key provided. Set ARES_API_KEY environment variable.")]
    NoApiKey,

    #[error("Invalid API key: {0}")]
    InvalidKey(String),

    #[error("Database error during auth: {0}")]
    DbError(#[from] ares_types::types::AppError),
}

/// Extracts the ARES API key from the environment.
///
/// MCP servers are spawned as child processes. The API key is passed via
/// the `ARES_API_KEY` environment variable.
pub fn extract_api_key_from_env() -> Result<String, McpAuthError> {
    std::env::var("ARES_API_KEY").map_err(|_| McpAuthError::NoApiKey)
}

/// Pure format check — does not query the database.
pub fn validate_api_key_format(api_key: &str) -> Result<(), McpAuthError> {
    if !api_key.starts_with("ares_") {
        return Err(McpAuthError::InvalidKey(
            "API key must start with 'ares_' prefix".to_string(),
        ));
    }
    Ok(())
}

#[cfg(feature = "postgres")]
/// Validates an API key against the tenant database and returns `TenantContext`.
pub async fn validate_mcp_api_key(
    tenant_db: &ares_store::tenants::TenantDb,
    api_key: &str,
) -> Result<TenantContext, McpAuthError> {
    validate_api_key_format(api_key)?;

    let tenant = tenant_db
        .verify_api_key(api_key)
        .await
        .map_err(|e| McpAuthError::InvalidKey(e.to_string()))?
        .ok_or_else(|| McpAuthError::InvalidKey("API key not found or inactive".to_string()))?;

    tracing::info!(
        tenant_id = %tenant.tenant_id,
        tier = %tenant.tier.as_str(),
        "MCP connection authenticated"
    );

    Ok(tenant)
}

#[cfg(feature = "postgres")]
/// Authenticated context for an MCP session (created once at connection time).
#[derive(Debug, Clone)]
pub struct McpSession {
    pub tenant: TenantContext,
    pub api_key: String,
    pub eruka_workspace_id: String,
}

#[cfg(feature = "postgres")]
impl McpSession {
    pub fn new(tenant: TenantContext, api_key: String) -> Self {
        let eruka_workspace_id = tenant.tenant_id.clone();
        Self {
            tenant,
            api_key,
            eruka_workspace_id,
        }
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant.tenant_id
    }

    pub fn tier(&self) -> &str {
        self.tenant.tier.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "postgres")]
    use ares_types::{TenantContext, TenantTier};
    use ares_types::types::AppError;
    use std::sync::Mutex;

    /// Serializes tests that read `ARES_API_KEY` while another test may remove it.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn assert_invalid_prefix(api_key: &str) {
        let err = validate_api_key_format(api_key).unwrap_err();
        match err {
            McpAuthError::InvalidKey(msg) => {
                assert_eq!(msg, "API key must start with 'ares_' prefix");
            }
            other => panic!("expected InvalidKey, got {other:?}"),
        }
    }

    #[test]
    fn validate_api_key_format_requires_ares_prefix() {
        assert!(validate_api_key_format("sk-live-abc").is_err());
        assert!(validate_api_key_format("ares_abcdefgh12345678").is_ok());
    }

    #[test]
    fn validate_api_key_format_invalid_returns_prefix_message() {
        assert_invalid_prefix("sk-live-abc");
        assert_invalid_prefix("not-a-key");
    }

    #[test]
    fn validate_api_key_format_almost_valid_prefixes_fail() {
        assert_invalid_prefix("");
        assert_invalid_prefix("ares");
        assert_invalid_prefix("are_");
        assert_invalid_prefix("aresx_");
        assert_invalid_prefix(" ares_key");
        assert_invalid_prefix("\tares_key");
        assert_invalid_prefix("Ares_key");
    }

    #[test]
    fn validate_api_key_format_valid_prefixes_pass() {
        assert!(validate_api_key_format("ares_").is_ok());
        assert!(validate_api_key_format("ares_a").is_ok());
        assert!(validate_api_key_format("ares_0123456789abcdef").is_ok());
    }

    #[test]
    fn extract_api_key_from_env_reads_variable_when_set() {
        let Ok(expected) = std::env::var("ARES_API_KEY") else {
            return;
        };
        let _guard = ENV_LOCK.lock().expect("env lock");
        let key = extract_api_key_from_env().expect("key");
        assert_eq!(key, expected);
    }

    #[test]
    fn extract_api_key_from_env_errors_when_var_already_unset() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        if std::env::var("ARES_API_KEY").is_ok() {
            return;
        }
        let result = extract_api_key_from_env();
        assert!(matches!(result, Err(McpAuthError::NoApiKey)));
    }

    #[test]
    fn extract_api_key_from_env_empty_value_returns_empty_string() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let old = std::env::var("ARES_API_KEY").ok();
        std::env::set_var("ARES_API_KEY", "");
        let result = extract_api_key_from_env().expect("should return Ok for empty value");
        assert_eq!(result, "");
        match old {
            Some(v) => std::env::set_var("ARES_API_KEY", v),
            None => std::env::remove_var("ARES_API_KEY"),
        }
    }

    #[test]
    fn extract_api_key_from_env_missing_under_guard_returns_error() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let old = std::env::var("ARES_API_KEY").ok();
        std::env::remove_var("ARES_API_KEY");
        let result = extract_api_key_from_env();
        assert!(matches!(result, Err(McpAuthError::NoApiKey)));
        match old {
            Some(v) => std::env::set_var("ARES_API_KEY", v),
            None => std::env::remove_var("ARES_API_KEY"),
        }
    }

    #[test]
    fn validate_api_key_format_malformed_suffix_with_valid_prefix() {
        // The validator only checks prefix; these "malformed" suffixes are accepted.
        assert!(validate_api_key_format("ares_\0null").is_ok());
        assert!(validate_api_key_format("ares_   spaces").is_ok());
        assert!(validate_api_key_format("ares_\t\t").is_ok());
        assert!(validate_api_key_format("ares_\n").is_ok());
        assert!(validate_api_key_format("ares_🔑").is_ok());
    }

    #[test]
    fn validate_api_key_format_rejects_malformed_prefix_variants() {
        assert_invalid_prefix("are_s_");
        assert_invalid_prefix("aRes_key");
        assert_invalid_prefix("Ares_key");
        assert_invalid_prefix(" ares_key");
        assert_invalid_prefix("ares-");
        assert_invalid_prefix("ares");
    }

    #[test]
    fn validate_api_key_format_edge_cases() {
        // Empty string
        assert!(validate_api_key_format("").is_err());
        // Exactly "ares_" with no suffix
        assert!(validate_api_key_format("ares_").is_ok());
        // No prefix
        assert!(validate_api_key_format("random-key").is_err());
        // Wrong prefix (case sensitive)
        assert!(validate_api_key_format("Ares_key").is_err());
        // Very long key
        let long_key = format!("ares_{}", "x".repeat(10_000));
        assert!(validate_api_key_format(&long_key).is_ok());
        // Unicode — starts with ares_ so valid
        assert!(validate_api_key_format("ares_🔑unicode").is_ok());
        // Unicode that doesn't start with ares_
        assert!(validate_api_key_format("🔑notares").is_err());
    }

    #[test]
    fn mcp_auth_error_display_messages() {
        assert_eq!(
            McpAuthError::NoApiKey.to_string(),
            "No API key provided. Set ARES_API_KEY environment variable."
        );
        assert_eq!(
            McpAuthError::InvalidKey("bad prefix".into()).to_string(),
            "Invalid API key: bad prefix"
        );
        let db_err: McpAuthError =
            AppError::Database("connection refused".into()).into();
        assert_eq!(
            db_err.to_string(),
            "Database error during auth: Database error: connection refused"
        );
    }

    #[test]
    fn mcp_auth_error_debug_includes_variant_name() {
        let err = McpAuthError::NoApiKey;
        let debug = format!("{err:?}");
        assert!(debug.contains("NoApiKey"));
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn mcp_session_new_and_accessors() {
        let ctx = TenantContext::new("test-tenant-001".into(), TenantTier::Free);
        let session = McpSession::new(ctx, "ares_testkey12345678".into());
        assert_eq!(session.tenant_id(), "test-tenant-001");
        assert_eq!(session.tier(), "free");
        assert_eq!(session.api_key, "ares_testkey12345678");
        assert_eq!(session.eruka_workspace_id, "test-tenant-001");
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn mcp_session_clone_matches() {
        let ctx = TenantContext::new("tenant-clone-test".into(), TenantTier::Pro);
        let session = McpSession::new(ctx, "ares_clonekey".into());
        let cloned = session.clone();
        assert_eq!(cloned.tenant_id(), session.tenant_id());
        assert_eq!(cloned.tier(), session.tier());
        assert_eq!(cloned.api_key, session.api_key);
        assert_eq!(cloned.eruka_workspace_id, session.eruka_workspace_id);
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn mcp_session_tier_reflects_tenant_context() {
        let ctx = TenantContext::new("tenant-enterprise".into(), TenantTier::Enterprise);
        let session = McpSession::new(ctx, "ares_ent_key".into());
        assert_eq!(session.tier(), "enterprise");
        assert_eq!(session.eruka_workspace_id, "tenant-enterprise");
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn mcp_session_creation_and_quota_tracking_all_tiers() {
        let free = McpSession::new(
            TenantContext::new("t-free".into(), TenantTier::Free),
            "ares_free".into(),
        );
        assert_eq!(free.tier(), "free");
        assert_eq!(free.tenant.quota.requests_per_month, 1_000);
        assert_eq!(free.tenant.quota.tokens_per_month, 100_000);
        assert_eq!(free.tenant.quota.max_agents, 1);

        let dev = McpSession::new(
            TenantContext::new("t-dev".into(), TenantTier::Dev),
            "ares_dev".into(),
        );
        assert_eq!(dev.tier(), "dev");
        assert_eq!(dev.tenant.quota.requests_per_month, 50_000);
        assert_eq!(dev.tenant.quota.tokens_per_month, 5_000_000);

        let pro = McpSession::new(
            TenantContext::new("t-pro".into(), TenantTier::Pro),
            "ares_pro".into(),
        );
        assert_eq!(pro.tier(), "pro");
        assert_eq!(pro.tenant.quota.requests_per_month, 500_000);
        assert_eq!(pro.tenant.quota.tokens_per_month, 50_000_000);

        let ent = McpSession::new(
            TenantContext::new("t-ent".into(), TenantTier::Enterprise),
            "ares_ent".into(),
        );
        assert_eq!(ent.tier(), "enterprise");
        assert_eq!(ent.tenant.quota.requests_per_month, u64::MAX);
        assert_eq!(ent.tenant.quota.tokens_per_month, u64::MAX);
        assert_eq!(ent.tenant.quota.max_agents, u32::MAX);
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn mcp_session_tier_limits_enforce_request_quotas() {
        let free = McpSession::new(
            TenantContext::new("t".into(), TenantTier::Free),
            "ares_test".into(),
        );
        assert!(free.tenant.can_make_request(0, 0));
        assert!(free.tenant.can_make_request(999, 49));
        assert!(!free.tenant.can_make_request(1_000, 0));
        assert!(!free.tenant.can_make_request(0, 50));

        let pro = McpSession::new(
            TenantContext::new("t".into(), TenantTier::Pro),
            "ares_test".into(),
        );
        assert!(pro.tenant.can_make_request(499_999, 19_999));
        assert!(!pro.tenant.can_make_request(500_000, 0));
        assert!(!pro.tenant.can_make_request(0, 20_000));

        let ent = McpSession::new(
            TenantContext::new("t".into(), TenantTier::Enterprise),
            "ares_test".into(),
        );
        assert!(ent.tenant.can_make_request(u64::MAX - 1, u64::MAX - 1));
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn mcp_session_tier_limits_enforce_token_quotas() {
        let free = McpSession::new(
            TenantContext::new("t".into(), TenantTier::Free),
            "ares_test".into(),
        );
        assert!(free.tenant.can_use_tokens(0, 100_000));
        assert!(!free.tenant.can_use_tokens(100_000, 1));
        assert!(!free.tenant.can_use_tokens(u64::MAX, 1));

        let pro = McpSession::new(
            TenantContext::new("t".into(), TenantTier::Pro),
            "ares_test".into(),
        );
        assert!(pro.tenant.can_use_tokens(0, 50_000_000));
        assert!(!pro.tenant.can_use_tokens(50_000_000, 1));

        let ent = McpSession::new(
            TenantContext::new("t".into(), TenantTier::Enterprise),
            "ares_test".into(),
        );
        assert!(ent.tenant.can_use_tokens(u64::MAX - 1, 1));
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn mcp_session_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<McpSession>();
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn mcp_session_concurrent_clone_access() {
        let ctx = TenantContext::new("concurrent".into(), TenantTier::Pro);
        let session = McpSession::new(ctx, "ares_concurrent".into());
        std::thread::scope(|s| {
            for _ in 0..10 {
                let cloned = session.clone();
                s.spawn(move || {
                    assert_eq!(cloned.tier(), "pro");
                    assert_eq!(cloned.tenant_id(), "concurrent");
                    assert_eq!(cloned.api_key, "ares_concurrent");
                });
            }
        });
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn mcp_session_lifecycle_create_clone_drop() {
        let ctx = TenantContext::new("lifecycle".into(), TenantTier::Dev);
        let session = McpSession::new(ctx, "ares_lifecycle".into());
        let cloned = session.clone();
        drop(cloned);
        assert_eq!(session.tier(), "dev");
        assert_eq!(session.tenant_id(), "lifecycle");
    }

    #[ignore] // flaky: env-var race condition in parallel execution across crates
    #[test]
    fn extract_api_key_from_env_missing_returns_error() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::remove_var("ARES_API_KEY");
        let result = extract_api_key_from_env();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), McpAuthError::NoApiKey));
    }

    #[cfg(feature = "postgres")]
    mod validate_mcp_api_key_tests {
        use super::*;
        use ares_store::PostgresClient;
        use std::sync::Arc;

        fn test_tenant_db() -> ares_store::TenantDb {
            ares_store::TenantDb::new(Arc::new(PostgresClient::new_test()))
        }

        #[tokio::test]
        async fn rejects_invalid_format_before_database_lookup() {
            let err = validate_mcp_api_key(&test_tenant_db(), "not-a-key")
                .await
                .unwrap_err();
            match err {
                McpAuthError::InvalidKey(msg) => {
                    assert_eq!(msg, "API key must start with 'ares_' prefix");
                }
                other => panic!("expected InvalidKey, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn rejects_key_with_short_suffix_without_database() {
            let err = validate_mcp_api_key(&test_tenant_db(), "ares_short")
                .await
                .unwrap_err();
            match err {
                McpAuthError::InvalidKey(msg) => {
                    assert_eq!(msg, "API key not found or inactive");
                }
                other => panic!("expected InvalidKey, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn maps_database_lookup_errors_to_invalid_key() {
            let err = validate_mcp_api_key(&test_tenant_db(), "ares_abcdefgh12345678")
                .await
                .unwrap_err();
            match err {
                McpAuthError::InvalidKey(msg) => {
                    assert!(
                        msg.contains("Failed to lookup API key"),
                        "unexpected message: {msg}"
                    );
                }
                other => panic!("expected InvalidKey, got {other:?}"),
            }
        }
    }

}

