//! Slack connector tools.

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

const SLACK_TOKEN_URL: &str = "https://slack.com/api/oauth.v2.access";
const SLACK_BASE_URL: &str = "https://slack.com/api";

// =============================================================================
// HTTP client
// =============================================================================

#[derive(Debug, Clone)]
pub struct SlackClient {
    config: ConnectorConfig,
    http: reqwest::Client,
    pool: PgPool,
    master_key: MasterKey,
}

impl SlackClient {
    pub fn new(pool: PgPool, master_key: MasterKey) -> Self {
        Self {
            config: ConnectorConfig {
                base_url: SLACK_BASE_URL.to_string(),
                version: "v2".to_string(),
            },
            http: reqwest::Client::new(),
            pool,
            master_key,
        }
    }

    pub async fn access_token(&self, tenant_id: &str) -> Result<String> {
        // Slack uses the same OAuth2 refresh mechanism; note token_url may differ by flow.
        get_valid_access_token(
            &self.pool,
            &self.master_key,
            tenant_id,
            "slack",
            "oauth2",
            SLACK_TOKEN_URL,
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
        Ok(self.http.request(method, &url).bearer_auth(token))
    }

    pub async fn execute(
        &self,
        request: reqwest::RequestBuilder,
    ) -> std::result::Result<reqwest::Response, ConnectorError> {
        execute_with_retry(&self.http, request, "slack").await
    }

    /// Slack returns 200 OK with JSON `{ "ok": false, ... }` for errors.
    pub async fn parse_slack_response(&self, resp: reqwest::Response) -> Result<Value> {
        let body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("slack read body: {e}"))
        })?;
        let value: Value = serde_json::from_str(&body).map_err(|e| {
            ares_types::AppError::External(format!("slack parse failed: {e} (body: {body})"))
        })?;
        if value.get("ok").and_then(|v| v.as_bool()) == Some(false) {
            let error_msg = value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown slack error");
            return Err(ares_types::AppError::External(format!(
                "slack api error: {error_msg}"
            )));
        }
        Ok(value)
    }
}

// =============================================================================
// Data types
// =============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct SlackChannel {
    pub id: String,
    pub name: Option<String>,
    pub is_channel: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SlackMessageResponse {
    pub ok: bool,
    pub ts: Option<String>,
    pub channel: Option<String>,
    pub error: Option<String>,
}

// =============================================================================
// Send Message Tool
// =============================================================================

pub struct SlackSendMessage {
    client: SlackClient,
}

impl SlackSendMessage {
    pub fn new(client: SlackClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for SlackSendMessage {
    fn name(&self) -> &str {
        "slack_send_message"
    }

    fn description(&self) -> &str {
        "Send a message to a Slack channel"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"},
                "channel": {"type": "string", "description": "Channel ID or name"},
                "text": {"type": "string", "description": "Message text"}
            },
            "required": ["tenant_id", "channel", "text"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let channel = args["channel"].as_str().unwrap_or("");
        let text = args["text"].as_str().unwrap_or("");

        let req = self
            .client
            .request(&tenant_id, reqwest::Method::POST, "/chat.postMessage")
            .await?
            .json(&json!({"channel": channel, "text": text}));

        let resp = self.client.execute(req).await.map_err(|e| e.into())?;
        let data = self.client.parse_slack_response(resp).await?;
        Ok(data)
    }
}

// =============================================================================
// List Channels Tool
// =============================================================================

pub struct SlackListChannels {
    client: SlackClient,
}

impl SlackListChannels {
    pub fn new(client: SlackClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for SlackListChannels {
    fn name(&self) -> &str {
        "slack_list_channels"
    }

    fn description(&self) -> &str {
        "List public Slack channels"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"}
            },
            "required": ["tenant_id"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;

        let req = self
            .client
            .request(&tenant_id, reqwest::Method::GET, "/conversations.list")
            .await?;

        let resp = self.client.execute(req).await.map_err(|e| e.into())?;
        let data = self.client.parse_slack_response(resp).await?;
        Ok(data)
    }
}

// =============================================================================
// Upload File Tool
// =============================================================================

pub struct SlackUploadFile {
    client: SlackClient,
}

impl SlackUploadFile {
    pub fn new(client: SlackClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for SlackUploadFile {
    fn name(&self) -> &str {
        "slack_upload_file"
    }

    fn description(&self) -> &str {
        "Upload a file to a Slack channel"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"},
                "channel": {"type": "string", "description": "Channel ID or name"},
                "content": {"type": "string", "description": "File content (base64-encoded or plain text)"},
                "filename": {"type": "string", "description": "Filename"}
            },
            "required": ["tenant_id", "channel", "content", "filename"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let channel = args["channel"].as_str().unwrap_or("");
        let content = args["content"].as_str().unwrap_or("").as_bytes();
        let filename = args["filename"].as_str().unwrap_or("file.txt");

        let form = reqwest::multipart::Form::new()
            .text("channels", channel.to_string())
            .text("filename", filename.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(content.to_vec()).file_name(filename.to_string()),
            );

        let req = self
            .client
            .request(&tenant_id, reqwest::Method::POST, "/files.upload")
            .await?
            .multipart(form);

        let resp = self.client.execute(req).await.map_err(|e| e.into())?;
        let data = self.client.parse_slack_response(resp).await?;
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
    fn slack_tools_compile() {
        let _ = std::any::type_name::<SlackSendMessage>();
        let _ = std::any::type_name::<SlackListChannels>();
        let _ = std::any::type_name::<SlackUploadFile>();
    }
}
