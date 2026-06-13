//! Pre-built connectors for third-party services.
//!
//! Each connector is exposed as one or more ARES tools that agents can invoke.
//! All connectors authenticate via OAuth2 credentials stored in the database
//! (`oauth_credentials` table) and encrypt/decrypt using the fleet master key.

use ares_config::fleet_secrets::{decrypt_api_key, MasterKey};
use ares_db::oauth_credentials::{OAuthCredential, OAuthCredentialStore};
use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::time::Duration;

// =============================================================================
// Error handling
// =============================================================================

/// Connector-specific error wrapper.
#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("Auth failed for {provider}: {message}")]
    Auth { provider: String, message: String },
    #[error("HTTP error for {provider}: {status} — {message}")]
    Http { provider: String, status: u16, message: String },
    #[error("Rate limited by {provider}: {message}")]
    RateLimited { provider: String, message: String },
    #[error("Configuration error for {provider}: {message}")]
    Config { provider: String, message: String },
}

impl From<ConnectorError> for AppError {
    fn from(e: ConnectorError) -> Self {
        match e {
            ConnectorError::Auth { provider, message } => {
                AppError::Auth(format!("{provider} auth failed: {message}"))
            }
            ConnectorError::Http { provider, status, message } => {
                AppError::External(format!("{provider} HTTP {status}: {message}"))
            }
            ConnectorError::RateLimited { provider, message } => {
                AppError::RateLimited(format!("{provider}: {message}"))
            }
            ConnectorError::Config { provider, message } => {
                AppError::Configuration(format!("{provider}: {message}"))
            }
        }
    }
}

// =============================================================================
// Auth helpers
// =============================================================================

/// Retrieve and validate an OAuth access token for a tenant + provider.
///
/// Returns `AppError::Auth` if no credential is found or if it has expired
/// (refresh is a future enhancement).
pub async fn get_access_token(
    pool: &PgPool,
    master_key: &MasterKey,
    tenant_id: &str,
    provider: &str,
    connector_type: &str,
) -> Result<String> {
    let store = OAuthCredentialStore::new(pool);
    let cred = store
        .get(tenant_id, provider, connector_type)
        .await?
        .ok_or_else(|| {
            AppError::Auth(format!(
                "No OAuth credential found for tenant '{tenant_id}' and provider '{provider}'"
            ))
        })?;

    let now = chrono::Utc::now().timestamp();
    if cred.expires_at.map(|e| e <= now).unwrap_or(false) {
        return Err(AppError::Auth(format!(
            "OAuth token for {provider} has expired (expires_at={expires_at:?}, now={now}). Re-authentication required.",
            provider = provider,
            expires_at = cred.expires_at,
            now = now
        )));
    }

    let at_payload = cred.access_token.ok_or_else(|| {
        AppError::Auth(format!(
            "OAuth credential for {provider} has no access token"
        ))
    })?;
    let token = decrypt_api_key(&at_payload, master_key)
        .map_err(|e| AppError::Auth(format!("Failed to decrypt access token: {e}")))?;
    Ok(token)
}

/// Full OAuth credential (used when refresh is needed).
pub async fn get_oauth_credential(
    pool: &PgPool,
    _master_key: &MasterKey,
    tenant_id: &str,
    provider: &str,
    connector_type: &str,
) -> Result<OAuthCredential> {
    let store = OAuthCredentialStore::new(pool);
    store
        .get(tenant_id, provider, connector_type)
        .await?
        .ok_or_else(|| {
            AppError::Auth(format!(
                "No OAuth credential found for tenant '{tenant_id}' and provider '{provider}'"
            ))
        })
}

// =============================================================================
// HTTP helpers
// =============================================================================

/// Retry configuration for rate-limited requests.
pub const MAX_RETRIES: u32 = 3;
pub const BASE_RETRY_DELAY_MS: u64 = 1000;

/// Execute a reqwest request with automatic retry on 429.
pub async fn execute_with_retry(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> std::result::Result<reqwest::Response, ConnectorError> {
    let mut delay = Duration::from_millis(BASE_RETRY_DELAY_MS);
    let mut attempt = 0;

    loop {
        let response = request
            .try_clone()
            .ok_or_else(|| ConnectorError::Config {
                provider: provider.to_string(),
                message: "request body is a stream and cannot be cloned for retry".to_string(),
            })?
            .send()
            .await
            .map_err(|e| ConnectorError::Http {
                provider: provider.to_string(),
                status: 0,
                message: format!("reqwest error: {e}"),
            })?;

        let status = response.status().as_u16();

        if response.status().is_success() {
            return Ok(response);
        }

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt < MAX_RETRIES {
            // Try to parse Retry-After header, otherwise use exponential backoff
            let sleep_dur = response
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(delay);

            tokio::time::sleep(sleep_dur).await;
            delay *= 2;
            attempt += 1;
            continue;
        }

        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<unreadable body>".to_string());

        return Err(ConnectorError::Http {
            provider: provider.to_string(),
            status,
            message: body,
        });
    }
}

/// Shared configuration for all connectors.
#[derive(Debug, Clone)]
pub struct ConnectorConfig {
    pub base_url: String,
    pub version: String,
}

/// Minimal JSON error body shape returned by many REST APIs.
#[derive(Debug, Deserialize)]
pub struct ApiErrorBody {
    pub error: Option<ApiErrorDetail>,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApiErrorDetail {
    pub message: Option<String>,
    pub code: Option<String>,
}

/// Extract a human-readable message from a JSON error body.
pub fn extract_error_message(body: &str) -> String {
    serde_json::from_str::<ApiErrorBody>(body)
        .ok()
        .and_then(|e| {
            e.error
                .and_then(|d| d.message)
                .or(e.message)
        })
        .unwrap_or_else(|| body.to_string())
}

// =============================================================================
// OAuth2 refresh helper
// =============================================================================

/// Refresh an OAuth2 access token using a standard token endpoint.
///
/// On success, updates the credential in the database and returns the new
/// access token.
pub async fn refresh_oauth2_token(
    pool: &PgPool,
    master_key: &MasterKey,
    tenant_id: &str,
    provider: &str,
    connector_type: &str,
    token_url: &str,
) -> Result<String> {
    let cred = get_oauth_credential(pool, master_key, tenant_id, provider, connector_type).await?;

    let refresh_token = cred.refresh_token.as_ref().ok_or_else(|| {
        AppError::Auth(format!("{provider} credential has no refresh token"))
    })?;
    let refresh_token_plain = decrypt_api_key(refresh_token, master_key)
        .map_err(|e| AppError::Auth(format!("Failed to decrypt refresh token: {e}")))?;

    let client_secret_plain = decrypt_api_key(&cred.client_secret, master_key)
        .map_err(|e| AppError::Auth(format!("Failed to decrypt client secret: {e}")))?;

    let client = reqwest::Client::new();
    let response = client
        .post(token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token_plain),
            ("client_id", &cred.client_id),
            ("client_secret", &client_secret_plain),
        ])
        .send()
        .await
        .map_err(|e| AppError::External(format!("{provider} refresh token request failed: {e}")))?;

    if !response.status().is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_default();
        return Err(AppError::Auth(format!(
            "{provider} token refresh failed: {body}"
        )));
    }

    #[derive(Debug, Deserialize)]
    struct RefreshResponse {
        access_token: String,
        expires_in: i64,
        #[serde(default)]
        refresh_token: Option<String>,
    }

    let refresh_data: RefreshResponse = response
        .json()
        .await
        .map_err(|e| AppError::External(format!("{provider} refresh token parse failed: {e}")))?;

    let now = chrono::Utc::now().timestamp();
    let expires_at = now + refresh_data.expires_in;
    let new_refresh_token = refresh_data.refresh_token.as_deref().unwrap_or(&refresh_token_plain);

    let store = OAuthCredentialStore::new(pool);
    store
        .update_tokens(
            &cred.id,
            &refresh_data.access_token,
            Some(new_refresh_token),
            expires_at,
        )
        .await?;

    Ok(refresh_data.access_token)
}

/// Get a valid access token, refreshing if near expiry.
pub async fn get_valid_access_token(
    pool: &PgPool,
    master_key: &MasterKey,
    tenant_id: &str,
    provider: &str,
    connector_type: &str,
    token_url: &str,
) -> Result<String> {
    let store = OAuthCredentialStore::new(pool);
    let cred = store
        .get(tenant_id, provider, connector_type)
        .await?
        .ok_or_else(|| {
            AppError::Auth(format!(
                "No OAuth credential found for tenant '{tenant_id}' and provider '{provider}'"
            ))
        })?;

    let now = chrono::Utc::now().timestamp();
    // Refresh if expires within 5 minutes
    if cred.expires_at.map(|e| e <= now + 300).unwrap_or(true) {
        return refresh_oauth2_token(pool, master_key, tenant_id, provider, connector_type, token_url).await;
    }

    let at_payload = cred.access_token.ok_or_else(|| {
        AppError::Auth(format!(
            "OAuth credential for {provider} has no access token"
        ))
    })?;
    decrypt_api_key(&at_payload, master_key)
        .map_err(|e| AppError::Auth(format!("Failed to decrypt access token: {e}")))
}

// =============================================================================
// Tenant-id extraction helper for tool args
// =============================================================================

/// Extract `tenant_id` from tool arguments. Returns `AppError::InvalidInput` when missing.
pub fn require_tenant_id(args: &serde_json::Value) -> Result<String> {
    args.get("tenant_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::InvalidInput("tenant_id is required".to_string()))
}
