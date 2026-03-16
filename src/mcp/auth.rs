// ares/src/mcp/auth.rs
// Extracts and validates API key from MCP connection configuration.
// The API key is passed as an environment variable when the MCP server process is spawned.

use crate::db::tenants::TenantDb;
use crate::models::TenantContext;
use std::sync::Arc;

/// Error type for MCP authentication.
#[derive(Debug, thiserror::Error)]
pub enum McpAuthError {
    #[error("No API key provided. Set ARES_API_KEY environment variable.")]
    NoApiKey,

    #[error("Invalid API key: {0}")]
    InvalidKey(String),

    #[error("Database error during auth: {0}")]
    DbError(#[from] crate::types::AppError),
}

/// Extracts the ARES API key from the environment.
///
/// MCP servers are spawned as child processes. The API key is passed via
/// the ARES_API_KEY environment variable, which is set in the MCP client
/// config (e.g., claude_desktop_config.json → env block).
///
/// # Returns
/// The raw API key string (starts with "ares_").
pub fn extract_api_key_from_env() -> Result<String, McpAuthError> {
    std::env::var("ARES_API_KEY").map_err(|_| McpAuthError::NoApiKey)
}

/// Validates an API key and returns the TenantContext.
///
/// This calls the same validation logic used by the HTTP API middleware.
/// The TenantContext contains tenant_id, tier, and quota info.
///
/// # Arguments
/// - `tenant_db`: Tenant database for key validation
/// - `api_key`: Raw API key string (e.g., "ares_abc123...")
///
/// # Returns
/// - `Ok(TenantContext)` if the key is valid and the tenant is active
/// - `Err(McpAuthError)` if the key is invalid, expired, or the tenant is suspended
pub async fn validate_mcp_api_key(
    tenant_db: &TenantDb,
    api_key: &str,
) -> Result<TenantContext, McpAuthError> {
    // Verify the key starts with the expected prefix
    if !api_key.starts_with("ares_") {
        return Err(McpAuthError::InvalidKey(
            "API key must start with 'ares_' prefix".to_string(),
        ));
    }

    // Use the shared validation logic from the tenant module.
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

/// Struct that holds the authenticated context for an MCP session.
/// Created once at connection time, reused for every tool call.
#[derive(Debug, Clone)]
pub struct McpSession {
    /// The validated tenant context
    pub tenant: TenantContext,
    /// The raw API key (for forwarding to Eruka if needed)
    pub api_key: String,
    /// Eruka workspace ID for this tenant (derived from tenant_id)
    pub eruka_workspace_id: String,
}

impl McpSession {
    /// Creates a new MCP session from a validated tenant context.
    pub fn new(tenant: TenantContext, api_key: String) -> Self {
        // Convention: Eruka workspace ID = tenant_id
        let eruka_workspace_id = tenant.tenant_id.clone();

        Self {
            tenant,
            api_key,
            eruka_workspace_id,
        }
    }

    /// Returns the tenant ID for this session.
    pub fn tenant_id(&self) -> &str {
        &self.tenant.tenant_id
    }

    /// Returns the tenant tier (Free, Dev, Pro, Enterprise).
    pub fn tier(&self) -> &str {
        self.tenant.tier.as_str()
    }
}
