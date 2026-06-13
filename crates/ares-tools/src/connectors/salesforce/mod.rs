//! Salesforce connector tools.

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

// Salesforce token URL is instance-specific; we use the standard login URL.
const SALESFORCE_TOKEN_URL: &str = "https://login.salesforce.com/services/oauth2/token";

// =============================================================================
// HTTP client
// =============================================================================

#[derive(Debug, Clone)]
pub struct SalesforceClient {
    config: ConnectorConfig,
    http: reqwest::Client,
    pool: PgPool,
    master_key: MasterKey,
}

impl SalesforceClient {
    pub fn new(pool: PgPool, master_key: MasterKey) -> Self {
        Self {
            config: ConnectorConfig {
                base_url: "https://login.salesforce.com".to_string(),
                version: "v59.0".to_string(),
            },
            http: reqwest::Client::new(),
            pool,
            master_key,
        }
    }

    /// Get the Salesforce instance URL from the stored credential metadata.
    /// For now we default to the base URL; in production the instance URL
    /// should be stored alongside the token.
    pub async fn instance_url(&self, _tenant_id: &str) -> Result<String> {
        // TODO: store instance_url in oauth_credentials and read it here.
        Ok("https://login.salesforce.com".to_string())
    }

    pub async fn access_token(&self, tenant_id: &str) -> Result<String> {
        get_valid_access_token(
            &self.pool,
            &self.master_key,
            tenant_id,
            "salesforce",
            "oauth2",
            SALESFORCE_TOKEN_URL,
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
        let base = self.instance_url(tenant_id).await?;
        let url = format!("{}/services/data/{}{}", base, self.config.version, path);
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
        execute_with_retry(&self.http, request, "salesforce").await
    }
}

// =============================================================================
// SOQL Query Tool
// =============================================================================

pub struct SalesforceSoqlQuery {
    client: SalesforceClient,
}

impl SalesforceSoqlQuery {
    pub fn new(client: SalesforceClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for SalesforceSoqlQuery {
    fn name(&self) -> &str {
        "salesforce_soql_query"
    }

    fn description(&self) -> &str {
        "Execute a Salesforce SOQL query"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"},
                "query": {"type": "string", "description": "SOQL query string"}
            },
            "required": ["tenant_id", "query"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let query = args["query"].as_str().unwrap_or("");

        let req = self
            .client
            .request(&tenant_id, reqwest::Method::GET, &format!("/query?q={}", urlencoding::encode(query)))
            .await?;

        let resp = self.client.execute(req).await.map_err(|e| e.into())?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("salesforce soql read body: {e}"))
        })?;

        let value: Value = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "salesforce soql parse failed: {e} (body: {resp_body})"
            ))
        })?;

        Ok(value)
    }
}

// =============================================================================
// Get Record Tool
// =============================================================================

pub struct SalesforceGetRecord {
    client: SalesforceClient,
}

impl SalesforceGetRecord {
    pub fn new(client: SalesforceClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for SalesforceGetRecord {
    fn name(&self) -> &str {
        "salesforce_get_record"
    }

    fn description(&self) -> &str {
        "Get a Salesforce record by object type and ID"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"},
                "object": {"type": "string", "description": "SObject type (e.g. Account, Contact)"},
                "id": {"type": "string", "description": "Record ID"}
            },
            "required": ["tenant_id", "object", "id"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let object = args["object"].as_str().unwrap_or("");
        let id = args["id"].as_str().unwrap_or("");

        let path = format!("/sobjects/{}/{}", urlencoding::encode(object), urlencoding::encode(id));
        let req = self
            .client
            .request(&tenant_id, reqwest::Method::GET, &path)
            .await?;

        let resp = self.client.execute(req).await.map_err(|e| e.into())?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("salesforce get record read body: {e}"))
        })?;

        let value: Value = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "salesforce get record parse failed: {e} (body: {resp_body})"
            ))
        })?;

        Ok(json!({ "record": value }))
    }
}

// =============================================================================
// Create Record Tool
// =============================================================================

pub struct SalesforceCreateRecord {
    client: SalesforceClient,
}

impl SalesforceCreateRecord {
    pub fn new(client: SalesforceClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for SalesforceCreateRecord {
    fn name(&self) -> &str {
        "salesforce_create_record"
    }

    fn description(&self) -> &str {
        "Create a Salesforce record"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"},
                "object": {"type": "string", "description": "SObject type (e.g. Account, Contact)"},
                "fields": {"type": "object", "description": "Record fields"}
            },
            "required": ["tenant_id", "object", "fields"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let object = args["object"].as_str().unwrap_or("");
        let fields = args["fields"].clone();

        let path = format!("/sobjects/{}", urlencoding::encode(object));
        let req = self
            .client
            .request(&tenant_id, reqwest::Method::POST, &path)
            .await?
            .json(&fields);

        let resp = self.client.execute(req).await.map_err(|e| e.into())?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("salesforce create record read body: {e}"))
        })?;

        let value: Value = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "salesforce create record parse failed: {e} (body: {resp_body})"
            ))
        })?;

        Ok(json!({ "record": value }))
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salesforce_tools_compile() {
        let _ = std::any::type_name::<SalesforceSoqlQuery>();
        let _ = std::any::type_name::<SalesforceGetRecord>();
        let _ = std::any::type_name::<SalesforceCreateRecord>();
    }
}
