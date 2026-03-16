//! Welcome email stub for DSprint activation.
//! Actual SMTP sending will be implemented in T37 (lettre crate).

/// Send welcome email with DSprint results and credentials.
/// Currently a stub that logs the email content.
pub async fn send_welcome_email(
    email: &str,
    api_key: &str,
    workspace_id: &str,
    tier: &str,
) -> anyhow::Result<()> {
    tracing::info!(
        "[EMAIL STUB] Welcome email for {}:\n         - Workspace: {}\n         - Tier: {}\n         - API Key: {}...\n         - Portal: portal.dirmacs.com\n         - MCP: eruka.dirmacs.com/mcp\n         (Actual email sending via lettre in T37)",
        email,
        workspace_id,
        tier,
        &api_key[..api_key.len().min(12)]
    );
    Ok(())
}
