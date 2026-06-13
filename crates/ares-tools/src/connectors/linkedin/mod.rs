//! LinkedIn connector tools.

use crate::connectors::{
    execute_with_retry, get_valid_access_token, require_tenant_id, ConnectorConfig, ConnectorError,
};
use crate::registry::Tool;
use ares_config::fleet_secrets::MasterKey;
use ares_types::types::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;

const LINKEDIN_TOKEN_URL: &str = "https://www.linkedin.com/oauth/v2/accessToken";
const LINKEDIN_BASE_URL: &str = "https://api.linkedin.com/v2";

// =============================================================================
// HTTP client
// =============================================================================

#[derive(Debug, Clone)]
pub struct LinkedInClient {
    config: ConnectorConfig,
    http: reqwest::Client,
    pool: PgPool,
    master_key: MasterKey,
}

impl LinkedInClient {
    pub fn new(pool: PgPool, master_key: MasterKey) -> Self {
        Self {
            config: ConnectorConfig {
                base_url: LINKEDIN_BASE_URL.to_string(),
                version: "v2".to_string(),
            },
            http: reqwest::Client::new(),
            pool,
            master_key,
        }
    }

    pub async fn access_token(&self, tenant_id: &str) -> Result<String> {
        get_valid_access_token(
            &self.pool,
            &self.master_key,
            tenant_id,
            "linkedin",
            LINKEDIN_TOKEN_URL,
        )
        .await
    }

    pub async fn request(
        &self,
        tenant_id: &str,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder> {
        let token = self.access_token(tenant_id).await?;
        let url = format!("{}{}", self.config.base_url, path);
        Ok(self
            .http
            .request(method, &url)
            .bearer_auth(token)
            .header("X-Restli-Protocol-Version", "2.0.0"))
    }

    pub async fn execute(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<reqwest::Response, ConnectorError> {
        execute_with_retry(&self.http, request, "linkedin").await
    }
}

// =============================================================================
// Data types
// =============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct ShareResponse {
    pub id: Option<String>,
    pub activity: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Update {
    pub id: String,
    pub text: Option<String>,
    pub created_time: Option<i64>,
}

// =============================================================================
// Create Share Tool
// =============================================================================

pub struct LinkedInCreateShare {
    client: LinkedInClient,
}

impl LinkedInCreateShare {
    pub fn new(client: LinkedInClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for LinkedInCreateShare {
    fn name(&self) -> &str {
        "linkedin_create_share"
    }

    fn description(&self) -> &str {
        "Create a LinkedIn share (text post)"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"},
                "text": {"type": "string", "description": "Post text content"},
                "visibility": {"type": "string", "description": "Visibility: 'PUBLIC' or 'CONNECTIONS' (default PUBLIC)"}
            },
            "required": ["tenant_id", "text"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let text = args["text"].as_str().unwrap_or("");
        let visibility = args["visibility"].as_str().unwrap_or("PUBLIC");

        let body = json!({
            "author": "urn:li:person:me",
            "lifecycleState": "PUBLISHED",
            "specificContent": {
                "com.linkedin.ugc.ShareContent": {
                    "shareCommentary": { "text": text },
                    "shareMediaCategory": "NONE"
                }
            },
            "visibility": {
                "com.linkedin.ugc.MemberNetworkVisibility": visibility
            }
        });

        let req = self
            .client
            .request(&tenant_id, reqwest::Method::POST, "/ugcPosts")
            .await?
            .json(&body);

        let resp = self.client.execute(req).await.map_err(|e| e.into())?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("linkedin create share read body: {e}"))
        })?;

        let share: ShareResponse = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "linkedin create share parse failed: {e} (body: {resp_body})"
            ))
        })?;

        Ok(json!({ "share": share }))
    }
}

// =============================================================================
// Get Company Updates Tool
// =============================================================================

pub struct LinkedInGetCompanyUpdates {
    client: LinkedInClient,
}

impl LinkedInGetCompanyUpdates {
    pub fn new(client: LinkedInClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for LinkedInGetCompanyUpdates {
    fn name(&self) -> &str {
        "linkedin_get_company_updates"
    }

    fn description(&self) -> &str {
        "Get updates for a LinkedIn company"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"},
                "company_id": {"type": "string", "description": "LinkedIn company ID (numeric)"}
            },
            "required": ["tenant_id", "company_id"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let company_id = args["company_id"].as_str().unwrap_or("");

        let path = format!(
            "/ugcPosts?q=authors&authors=List(urn%3Ali%3Aorganization%3A{})",
            urlencoding::encode(company_id)
        );
        let req = self
            .client
            .request(&tenant_id, reqwest::Method::GET, &path)
            .await?;

        let resp = self.client.execute(req).await.map_err(|e| e.into())?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!(
                "linkedin get company updates read body: {e}"
            ))
        })?;

        let data: Value = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "linkedin get company updates parse failed: {e} (body: {resp_body})"
            ))
        })?;

        Ok(data)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linkedin_tools_compile() {
        let _ = std::any::type_name::<LinkedInCreateShare>();
        let _ = std::any::type_name::<LinkedInGetCompanyUpdates>();
    }
}
