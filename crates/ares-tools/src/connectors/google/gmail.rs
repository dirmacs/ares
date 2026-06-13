//! Gmail connector tools.

use crate::connectors::google::GoogleClient;
use crate::connectors::require_tenant_id;
use crate::registry::Tool;
use ares_types::types::{AppError, Result};
use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// =============================================================================
// Data types
// =============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct GmailMessage {
    pub id: String,
    pub thread_id: Option<String>,
    pub snippet: Option<String>,
    pub payload: Option<MessagePayload>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessagePayload {
    pub headers: Vec<MessageHeader>,
    pub body: Option<MessageBody>,
    pub parts: Option<Vec<MessagePayload>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageBody {
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub size: Option<i32>,
}

// =============================================================================
// Send Email Tool
// =============================================================================

pub struct GmailSendEmail {
    client: GoogleClient,
}

impl GmailSendEmail {
    pub fn new(client: GoogleClient) -> Self {
        Self { client }
    }

    async fn send_email(
        &self,
        tenant_id: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> Result<GmailMessage> {
        let raw = format!("To: {}\r\nSubject: {}\r\n\r\n{}\r\n", to, subject, body);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);
        let payload = json!({ "raw": encoded });

        let req = self
            .client
            .request(tenant_id, reqwest::Method::POST, "/users/me/messages/send")
            .await?
            .json(&payload);

        let resp = self.client.execute(req).await.map_err(AppError::from)?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("gmail send email read body: {e}"))
        })?;

        serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "gmail send email parse failed: {e} (body: {resp_body})"
            ))
        })
    }
}

#[async_trait]
impl Tool for GmailSendEmail {
    fn name(&self) -> &str {
        "gmail_send_email"
    }

    fn description(&self) -> &str {
        "Send an email via Gmail"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"},
                "to": {"type": "string", "description": "Recipient email address"},
                "subject": {"type": "string", "description": "Email subject"},
                "body": {"type": "string", "description": "Plain text body"}
            },
            "required": ["tenant_id", "to", "subject", "body"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let to = args["to"].as_str().unwrap_or("");
        let subject = args["subject"].as_str().unwrap_or("");
        let body = args["body"].as_str().unwrap_or("");

        let msg = self.send_email(&tenant_id, to, subject, body).await?;
        Ok(json!({ "message": msg }))
    }
}

// =============================================================================
// List Messages Tool
// =============================================================================

pub struct GmailListMessages {
    client: GoogleClient,
}

impl GmailListMessages {
    pub fn new(client: GoogleClient) -> Self {
        Self { client }
    }

    async fn list_messages(
        &self,
        tenant_id: &str,
        query: Option<&str>,
        max_results: i64,
    ) -> Result<Vec<GmailMessage>> {
        let mut req = self
            .client
            .request(tenant_id, reqwest::Method::GET, "/users/me/messages")
            .await?;
        if let Some(q) = query {
            req = req.query(&[("q", q)]);
        }
        req = req.query(&[("maxResults", &max_results.to_string())]);

        let resp = self.client.execute(req).await.map_err(AppError::from)?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("gmail list messages read body: {e}"))
        })?;

        #[derive(Debug, Deserialize)]
        struct MessageList {
            messages: Option<Vec<GmailMessage>>,
        }

        let list: MessageList = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "gmail list messages parse failed: {e} (body: {resp_body})"
            ))
        })?;

        Ok(list.messages.unwrap_or_default())
    }
}

#[async_trait]
impl Tool for GmailListMessages {
    fn name(&self) -> &str {
        "gmail_list_messages"
    }

    fn description(&self) -> &str {
        "List Gmail messages with an optional query"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"},
                "query": {"type": "string", "description": "Gmail search query (optional)"},
                "max_results": {"type": "integer", "description": "Maximum results (default 10)"}
            },
            "required": ["tenant_id"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let query = args["query"].as_str();
        let max_results = args["max_results"].as_i64().unwrap_or(10);

        let messages = self.list_messages(&tenant_id, query, max_results).await?;
        Ok(json!({ "messages": messages }))
    }
}

// =============================================================================
// Get Message Tool
// =============================================================================

pub struct GmailGetMessage {
    client: GoogleClient,
}

impl GmailGetMessage {
    pub fn new(client: GoogleClient) -> Self {
        Self { client }
    }

    async fn get_message(&self, tenant_id: &str, id: &str) -> Result<GmailMessage> {
        let path = format!("/users/me/messages/{}", urlencoding::encode(id));
        let req = self
            .client
            .request(tenant_id, reqwest::Method::GET, &path)
            .await?;

        let resp = self.client.execute(req).await.map_err(AppError::from)?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("gmail get message read body: {e}"))
        })?;

        serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "gmail get message parse failed: {e} (body: {resp_body})"
            ))
        })
    }
}

#[async_trait]
impl Tool for GmailGetMessage {
    fn name(&self) -> &str {
        "gmail_get_message"
    }

    fn description(&self) -> &str {
        "Get a Gmail message by ID"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tenant_id": {"type": "string", "description": "Tenant identifier"},
                "message_id": {"type": "string", "description": "Gmail message ID"}
            },
            "required": ["tenant_id", "message_id"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let id = args["message_id"].as_str().ok_or_else(|| {
            ares_types::AppError::InvalidInput("message_id is required".to_string())
        })?;

        let msg = self.get_message(&tenant_id, id).await?;
        Ok(json!({ "message": msg }))
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_tools_compile() {
        let _ = std::any::type_name::<GmailSendEmail>();
        let _ = std::any::type_name::<GmailListMessages>();
        let _ = std::any::type_name::<GmailGetMessage>();
    }
}
