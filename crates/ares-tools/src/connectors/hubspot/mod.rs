//! HubSpot connector tools.

use crate::connectors::{
    execute_with_retry, get_valid_access_token, require_tenant_id, ConnectorConfig, ConnectorError,
};
use crate::registry::Tool;
use ares_config::fleet_secrets::MasterKey;
use ares_types::types::{AppError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;

const HUBSPOT_TOKEN_URL: &str = "https://api.hubapi.com/oauth/v1/token";
const HUBSPOT_BASE_URL: &str = "https://api.hubapi.com";

// =============================================================================
// HTTP client
// =============================================================================

#[derive(Debug, Clone)]
pub struct HubSpotClient {
    config: ConnectorConfig,
    http: reqwest::Client,
    pool: PgPool,
    master_key: MasterKey,
}

impl HubSpotClient {
    pub fn new(pool: PgPool, master_key: MasterKey) -> Self {
        Self {
            config: ConnectorConfig {
                base_url: HUBSPOT_BASE_URL.to_string(),
                version: "v3".to_string(),
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
            "hubspot",
            "hubspot",
            HUBSPOT_TOKEN_URL,
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
            .header("Content-Type", "application/json"))
    }

    pub async fn execute(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<reqwest::Response, ConnectorError> {
        execute_with_retry(&self.http, request, "hubspot").await
    }
}

// =============================================================================
// Data types
// =============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    pub properties: Value,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Deal {
    pub id: String,
    pub properties: Value,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

// =============================================================================
// Get Contact Tool
// =============================================================================

pub struct HubSpotGetContact {
    client: HubSpotClient,
}

impl HubSpotGetContact {
    pub fn new(client: HubSpotClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for HubSpotGetContact {
    fn name(&self) -> &str {
        "hubspot_get_contact"
    }

    fn description(&self) -> &str {
        "Get a HubSpot contact by email"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "email": {"type": "string", "description": "Contact email address"}
            },
            "required": ["email"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let email = args["email"].as_str().unwrap_or("");

        let req = self
            .client
            .request(
                &tenant_id,
                reqwest::Method::GET,
                &format!(
                    "/crm/v3/objects/contacts/{}?idProperty=email",
                    urlencoding::encode(email)
                ),
            )
            .await?;

        let resp = self.client.execute(req).await.map_err(AppError::from)?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("hubspot get contact read body: {e}"))
        })?;

        let value: Value = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "hubspot get contact parse failed: {e} (body: {resp_body})"
            ))
        })?;

        if value.get("status").and_then(|v| v.as_str()) == Some("error") {
            let msg = value
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("hubspot error");
            return Ok(json!({ "contact": null, "error": msg }));
        }

        Ok(json!({ "contact": value }))
    }
}

// =============================================================================
// Create Contact Tool
// =============================================================================

pub struct HubSpotCreateContact {
    client: HubSpotClient,
}

impl HubSpotCreateContact {
    pub fn new(client: HubSpotClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for HubSpotCreateContact {
    fn name(&self) -> &str {
        "hubspot_create_contact"
    }

    fn description(&self) -> &str {
        "Create a HubSpot contact"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "properties": {"type": "object", "description": "Contact properties (e.g. email, firstname, lastname)"}
            },
            "required": ["properties"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let properties = args["properties"].clone();

        let body = json!({ "properties": properties });
        let req = self
            .client
            .request(
                &tenant_id,
                reqwest::Method::POST,
                "/crm/v3/objects/contacts",
            )
            .await?
            .json(&body);

        let resp = self.client.execute(req).await.map_err(AppError::from)?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("hubspot create contact read body: {e}"))
        })?;

        let value: Value = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "hubspot create contact parse failed: {e} (body: {resp_body})"
            ))
        })?;

        Ok(json!({ "contact": value }))
    }
}

// =============================================================================
// List Deals Tool
// =============================================================================

pub struct HubSpotListDeals {
    client: HubSpotClient,
}

impl HubSpotListDeals {
    pub fn new(client: HubSpotClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for HubSpotListDeals {
    fn name(&self) -> &str {
        "hubspot_list_deals"
    }

    fn description(&self) -> &str {
        "List HubSpot deals, optionally filtered by pipeline"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pipeline": {"type": "string", "description": "Pipeline name or ID (optional)"}
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let pipeline = args["pipeline"].as_str();

        let mut path = "/crm/v3/objects/deals?limit=100".to_string();
        if let Some(p) = pipeline {
            path.push_str(&format!(
                "&properties=pipeline&filter={}",
                urlencoding::encode(p)
            ));
        }

        let req = self
            .client
            .request(&tenant_id, reqwest::Method::GET, &path)
            .await?;

        let resp = self.client.execute(req).await.map_err(AppError::from)?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("hubspot list deals read body: {e}"))
        })?;

        let value: Value = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "hubspot list deals parse failed: {e} (body: {resp_body})"
            ))
        })?;

        Ok(value)
    }
}

// =============================================================================
// Create Deal Tool
// =============================================================================

pub struct HubSpotCreateDeal {
    client: HubSpotClient,
}

impl HubSpotCreateDeal {
    pub fn new(client: HubSpotClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for HubSpotCreateDeal {
    fn name(&self) -> &str {
        "hubspot_create_deal"
    }

    fn description(&self) -> &str {
        "Create a HubSpot deal"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "properties": {"type": "object", "description": "Deal properties (e.g. dealname, amount, pipeline)"}
            },
            "required": ["properties"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let properties = args["properties"].clone();

        let body = json!({ "properties": properties });
        let req = self
            .client
            .request(&tenant_id, reqwest::Method::POST, "/crm/v3/objects/deals")
            .await?
            .json(&body);

        let resp = self.client.execute(req).await.map_err(AppError::from)?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("hubspot create deal read body: {e}"))
        })?;

        let value: Value = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "hubspot create deal parse failed: {e} (body: {resp_body})"
            ))
        })?;

        Ok(json!({ "deal": value }))
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hubspot_tools_compile() {
        let _ = std::any::type_name::<HubSpotGetContact>();
        let _ = std::any::type_name::<HubSpotCreateContact>();
        let _ = std::any::type_name::<HubSpotListDeals>();
        let _ = std::any::type_name::<HubSpotCreateDeal>();
    }
}
