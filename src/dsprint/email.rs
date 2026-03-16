//! Welcome email stub for DSprint activation.
//! Actual SMTP sending will be implemented in T37 (lettre crate).

use anyhow::Result;
use lettre::{
    transport::smtp::authentication::Credentials, message::Mailbox, Message, SmtpTransport,
    Transport,
};
use std::env;

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

/// Send an email via SMTP.
/// If SMTP_HOST is not set, logs the email content instead of sending.
pub async fn send_email(
    to_email: &str,
    subject: &str,
    body: &str,
) -> Result<()> {
    // Read SMTP configuration from environment
    let smtp_host = env::var("SMTP_HOST").ok();

    if smtp_host.is_none() {
        tracing::info!(
            "[EMAIL DRY RUN] Would send email to: {}\nSubject: {}\nBody: {}",
            to_email,
            subject,
            body
        );
        return Ok(());
    }

    let smtp_host = smtp_host.unwrap();
    let smtp_port: u16 = env::var("SMTP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(587);

    let smtp_user = env::var("SMTP_USER").ok();
    let smtp_pass = env::var("SMTP_PASS").ok();
    let smtp_from = env::var("SMTP_FROM").ok();

    // Build the email message
    let from_email = smtp_from.unwrap_or_else(|| "noreply@localhost".to_string());
    let from = Mailbox::new(None, from_email.parse()?);
    let to = Mailbox::new(None, to_email.parse()?);

    let email_message = Message::builder()
        .from(from)
        .to(to)
        .subject(subject)
        .text(body)
        .build()?;

    // Build SMTP transport
    let mut builder = SmtpTransport::builder_dangerous(&smtp_host)
        .port(smtp_port)
        .tls(lettre::transport::smtp::client::Tls::Required(
            lettre::transport::smtp::client::TlsParameters::new(smtp_host.clone())?
        ));

    // Add credentials if provided
    if let (Some(user), Some(pass)) = (smtp_user, smtp_pass) {
        let credentials = Credentials::new(user, pass);
        builder = builder.credentials(credentials);
    }

    let mailer = builder.build();

    // Send the email
    match mailer.send(&email_message) {
        Ok(_) => {
            tracing::info!("Email sent successfully to {}", to_email);
            Ok(())
        }
        Err(e) => {
            anyhow::bail!("Failed to send email: {}", e)
        }
    }
}
