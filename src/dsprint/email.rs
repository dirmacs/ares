//! Welcome email template for DSprint activation.
//! Actual SMTP sending via lettre crate comes in T37.
//! For now, this just formats and logs the email content.

use anyhow::Result;

/// Format and "send" a welcome email after DSprint activation.
/// Currently a stub that logs the email content.
/// T37 will add real SMTP sending via lettre.
pub async fn send_welcome_email(
    email: &str,
    api_key: &str,
    workspace_id: &str,
    tenant_id: &str,
    agents: &[String],
) -> Result<()> {
    let agent_list = agents.join(", ");
    let email_body = format!(
        r#"Welcome to DIRMACS!

Your DSprint analysis is complete. Here are your credentials:

API Key: {}
Workspace: {}
Tenant ID: {}

Provisioned Agents: {}

Get started:
1. Enterprise Portal: https://portal.dirmacs.com
2. API Docs: https://api.ares.dirmacs.com/swagger-ui
3. MCP Setup: https://eruka.dirmacs.com/mcp

Connect your tools via MCP for the best experience.

— The DIRMACS Team"#,
        api_key, workspace_id, tenant_id, agent_list
    );

    tracing::info!(
        to = email,
        workspace_id = workspace_id,
        "Welcome email prepared (SMTP stub — actual sending in T37):\n{}\n",
        email_body
    );

    Ok(())
}
