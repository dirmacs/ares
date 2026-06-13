//! Google connector — shared OAuth and HTTP client for Google APIs.

use crate::connectors::{
    execute_with_retry, get_valid_access_token, ConnectorConfig, ConnectorError,
};
use ares_config::fleet_secrets::MasterKey;
use ares_types::types::Result;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

pub mod calendar;
pub mod gmail;

const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Shared Google API client.
#[derive(Debug, Clone)]
pub struct GoogleClient {
    config: ConnectorConfig,
    http: reqwest::Client,
    pool: PgPool,
    master_key: MasterKey,
}

impl GoogleClient {
    /// Create a new Google client.
    pub fn new(pool: PgPool, master_key: MasterKey) -> Self {
        Self {
            config: ConnectorConfig {
                base_url: "https://www.googleapis.com".to_string(),
                version: "v3".to_string(),
            },
            http: reqwest::Client::new(),
            pool,
            master_key,
        }
    }

    /// Get a valid access token for the given tenant, refreshing if needed.
    pub async fn access_token(&self, tenant_id: &str) -> Result<String> {
        get_valid_access_token(
            &self.pool,
            &self.master_key,
            tenant_id,
            "google",
            GOOGLE_TOKEN_URL,
        )
        .await
    }

    /// Build an authenticated request builder.
    pub async fn request(
        &self,
        tenant_id: &str,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder> {
        let token = self.access_token(tenant_id).await?;
        let url = format!("{}/{}{}", self.config.base_url, self.config.version, path);
        Ok(self
            .http
            .request(method, &url)
            .bearer_auth(token))
    }

    /// Execute a request with automatic retry on 429.
    pub async fn execute(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<reqwest::Response, ConnectorError> {
        execute_with_retry(&self.http, request, "google").await
    }
}

/// Common Google API error body.
#[derive(Debug, Deserialize)]
pub struct GoogleError {
    pub error: Option<GoogleErrorDetail>,
}

#[derive(Debug, Deserialize)]
pub struct GoogleErrorDetail {
    pub code: Option<i32>,
    pub message: Option<String>,
    pub status: Option<String>,
}
