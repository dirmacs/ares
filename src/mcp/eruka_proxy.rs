// ares/src/mcp/eruka_proxy.rs
// Proxy layer that forwards MCP tool calls to Eruka's HTTP API.

use crate::mcp::auth::McpSession;
use crate::mcp::tools::{
    ErukaReadInput, ErukaReadOutput,
    ErukaWriteInput, ErukaWriteOutput,
    ErukaSearchInput, ErukaSearchOutput, ErukaSearchResult,
};
use crate::types::AppError;
use serde_json::Value;

/// Error type for Eruka proxy operations.
#[derive(Debug, thiserror::Error)]
pub enum ErukaProxyError {
    #[error("Eruka HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Eruka returned error: {status} — {body}")]
    ApiError { status: u16, body: String },

    #[error("Failed to parse Eruka response: {0}")]
    Parse(String),

    #[error("Eruka is not configured or unreachable")]
    NotConfigured,
}

impl From<ErukaProxyError> for AppError {
    fn from(e: ErukaProxyError) -> Self {
        match e {
            ErukaProxyError::Http(e) => AppError::External(format!("Eruka HTTP error: {}", e)),
            ErukaProxyError::ApiError { status, body } => {
                AppError::External(format!("Eruka API error {}: {}", status, body))
            }
            ErukaProxyError::Parse(s) => AppError::External(format!("Eruka parse error: {}", s)),
            ErukaProxyError::NotConfigured => {
                AppError::External("Eruka not configured".to_string())
            }
        }
    }
}

/// Eruka proxy client.
/// Created once per MCP session, holds the HTTP client and Eruka base URL.
pub struct ErukaProxy {
    http: reqwest::Client,
    base_url: String,
}

impl ErukaProxy {
    /// Creates a new ErukaProxy.
    ///
    /// # Arguments
    /// - `eruka_base_url`: Base URL of the Eruka API (e.g., "https://eruka.dirmacs.com")
    pub fn new(eruka_base_url: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("Failed to build Eruka proxy HTTP client");

        Self {
            http,
            base_url: eruka_base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Reads a single field from Eruka.
    ///
    /// Calls: GET {eruka}/api/workspaces/{workspace_id}/context/{category}/{field}
    pub async fn read(
        &self,
        session: &McpSession,
        input: ErukaReadInput,
    ) -> Result<ErukaReadOutput, ErukaProxyError> {
        let workspace_id = input
            .workspace_id
            .as_deref()
            .unwrap_or(&session.eruka_workspace_id);

        let url = format!(
            "{}/api/workspaces/{}/context/{}/{}",
            self.base_url, workspace_id, input.category, input.field
        );

        let response = self.http.get(&url).send().await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(ErukaProxyError::ApiError { status, body });
        }

        let json: Value = response.json().await?;

        Ok(ErukaReadOutput {
            field: json["field"]
                .as_str()
                .unwrap_or(&input.field)
                .to_string(),
            value: json["value"].clone(),
            state: json["state"]
                .as_str()
                .unwrap_or("UNKNOWN")
                .to_string(),
            confidence: json["confidence"].as_f64().unwrap_or(0.0),
            last_updated: json["last_updated"].as_str().map(String::from),
        })
    }

    /// Writes a field to Eruka.
    ///
    /// Calls: POST {eruka}/api/workspaces/{workspace_id}/context
    pub async fn write(
        &self,
        session: &McpSession,
        input: ErukaWriteInput,
    ) -> Result<ErukaWriteOutput, ErukaProxyError> {
        let workspace_id = input
            .workspace_id
            .as_deref()
            .unwrap_or(&session.eruka_workspace_id);

        let url = format!(
            "{}/api/workspaces/{}/context",
            self.base_url, workspace_id
        );

        let body = serde_json::json!({
            "category": input.category,
            "field": input.field,
            "value": input.value,
            "confidence": input.confidence,
            "source": input.source
        });

        let response = self.http.post(&url).json(&body).send().await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(ErukaProxyError::ApiError {
                status,
                body: body_text,
            });
        }

        let json: Value = response.json().await?;

        let state = if input.confidence >= 1.0 {
            "CONFIRMED"
        } else {
            "UNCERTAIN"
        };

        Ok(ErukaWriteOutput {
            field: input.field,
            state: state.to_string(),
            written_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Searches Eruka knowledge base.
    ///
    /// Calls: POST {eruka}/api/workspaces/{workspace_id}/search
    pub async fn search(
        &self,
        session: &McpSession,
        input: ErukaSearchInput,
    ) -> Result<ErukaSearchOutput, ErukaProxyError> {
        let workspace_id = input
            .workspace_id
            .as_deref()
            .unwrap_or(&session.eruka_workspace_id);

        let url = format!(
            "{}/api/workspaces/{}/search",
            self.base_url, workspace_id
        );

        let body = serde_json::json!({
            "query": input.query,
            "limit": input.limit
        });

        let response = self.http.post(&url).json(&body).send().await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(ErukaProxyError::ApiError {
                status,
                body: body_text,
            });
        }

        let json: Value = response.json().await?;

        let results: Vec<ErukaSearchResult> = json["results"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|r| ErukaSearchResult {
                        category: r["category"].as_str().unwrap_or("").to_string(),
                        field: r["field"].as_str().unwrap_or("").to_string(),
                        value: r["value"].clone(),
                        state: r["state"].as_str().unwrap_or("UNKNOWN").to_string(),
                        relevance: r["relevance"].as_f64().unwrap_or(0.0),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let total = results.len();

        Ok(ErukaSearchOutput {
            results,
            total_results: total,
        })
    }
}
