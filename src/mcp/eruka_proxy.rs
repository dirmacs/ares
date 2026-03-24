// ares/src/mcp/eruka_proxy.rs
// Proxy layer that forwards MCP tool calls to Eruka's HTTP API.

use crate::mcp::auth::McpSession;
use crate::mcp::tools::{
    ErukaReadInput, ErukaReadOutput, ErukaSearchInput, ErukaSearchOutput, ErukaSearchResult,
    ErukaWriteInput, ErukaWriteOutput,
};
use crate::types::AppError;
use serde_json::Value;
use anyhow::Result;

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
    auth_token: Option<String>,
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

        // Try to get auth token from env
        let auth_token = std::env::var("ERUKA_AUTH_TOKEN").ok();

        Self {
            http,
            base_url: eruka_base_url.trim_end_matches('/').to_string(),
            auth_token,
        }
    }

    /// Create with explicit token
    pub fn with_token(eruka_base_url: &str, token: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("Failed to build Eruka proxy HTTP client");
        Self {
            http,
            base_url: eruka_base_url.trim_end_matches('/').to_string(),
            auth_token: Some(token.to_string()),
        }
    }

    /// Auto-login to Eruka and cache token
    pub async fn ensure_authenticated(&mut self) -> anyhow::Result<()> {
        if self.auth_token.is_some() { return Ok(()); }
        let email = std::env::var("ERUKA_SERVICE_EMAIL")
            .map_err(|_| anyhow::anyhow!("ERUKA_SERVICE_EMAIL env var is required"))?;
        let password = std::env::var("ERUKA_SERVICE_PASSWORD")
            .map_err(|_| anyhow::anyhow!("ERUKA_SERVICE_PASSWORD env var is required"))?;
        let url = format!("{}/api/v1/auth/login", self.base_url);
        let body = serde_json::json!({"email": email, "password": password});
        let resp = self.http.post(&url).json(&body).send().await?;
        if resp.status().is_success() {
            let json: serde_json::Value = resp.json().await?;
            if let Some(token) = json["token"].as_str() {
                self.auth_token = Some(token.to_string());
            }
        }
        Ok(())
    }

    pub fn auth_header(&self) -> Option<String> {
        self.auth_token.as_ref().map(|t| format!("Bearer {}", t))
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

        let mut req = self.http.get(&url);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let response = req.send().await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(ErukaProxyError::ApiError { status, body });
        }

        let json: Value = response.json().await?;

        Ok(ErukaReadOutput {
            field: json["field"].as_str().unwrap_or(&input.field).to_string(),
            value: json["value"].clone(),
            state: json["state"].as_str().unwrap_or("UNKNOWN").to_string(),
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

        let url = format!("{}/api/workspaces/{}/context", self.base_url, workspace_id);

        let body = serde_json::json!( {
            "category": input.category,
            "field": input.field,
            "value": input.value,
            "confidence": input.confidence,
            "source": input.source
        });

        let mut req = self.http.post(&url).json(&body);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let response = req.send().await?;

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

        let url = format!("{}/api/workspaces/{}/search", self.base_url, workspace_id);

        let body = serde_json::json!( {
            "query": input.query,
            "limit": input.limit
        });

        let mut req = self.http.post(&url).json(&body);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let response = req.send().await?;

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

    /// Creates a new workspace.
    ///
    /// POST /api/v1/workspaces
    pub async fn create_workspace(&self, name: &str, owner_email: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v1/workspaces", self.base_url);
        let body = serde_json::json!({"name": name, "owner_email": owner_email});
        let mut req = self.http.post(&url).json(&body);
        // Prefer service auth for DSprint (no bom JWT needed)
        if let Ok(service_key) = std::env::var("ERUKA_SERVICE_KEY") {
            req = req.header("X-Service-Key", &service_key);
            req = req.header("X-Workspace-Id", "system");
        } else if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Eruka API error {}: {}", status, body_text));
        }
        let json: serde_json::Value = response.json().await?;
        Ok(json)
    }

    /// Sends a chat message to Sisyphos.
    ///
    /// POST /api/v1/sisyphos/sessions/{session_id}/chat
    pub async fn sisyphos_chat(&self, session_id: &str, message: &str, workspace_id: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v1/sisyphos/sessions/{}/chat", self.base_url, session_id);
        let body = serde_json::json!({"message": message, "workspace_id": workspace_id});
        let mut req = self.http.post(&url).json(&body);
        // Use service auth with workspace_id so Sisyphos reads visitor's context, not bom's
        if let Ok(service_key) = std::env::var("ERUKA_SERVICE_KEY") {
            req = req.header("X-Service-Key", &service_key);
            req = req.header("X-Workspace-Id", workspace_id);
        } else if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Eruka API error {}: {}", status, body_text));
        }
        let json: serde_json::Value = response.json().await?;
        Ok(json)
    }

    /// Creates a new Sisyphos session.
    ///
    /// POST /api/v1/sisyphos/sessions
    pub async fn sisyphos_create_session(&self, user_id: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v1/sisyphos/sessions", self.base_url);
        let body = serde_json::json!({"user_id": user_id});
        let mut req = self.http.post(&url).json(&body);
        // Use service auth with workspace_id so Sisyphos reads the correct context
        if let Ok(service_key) = std::env::var("ERUKA_SERVICE_KEY") {
            req = req.header("X-Service-Key", &service_key);
            req = req.header("X-Workspace-Id", user_id);
        } else if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Eruka API error {}: {}", status, body_text));
        }
        let json: serde_json::Value = response.json().await?;
        Ok(json)
    }

    /// Retrieves gaps for a user.
    ///
    /// GET /api/v1/gaps?user_id=...
    pub async fn get_gaps(&self, user_id: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v1/gaps", self.base_url);
        let mut req = self.http.get(&url).query(&[("user_id", user_id)]);
        if let Ok(service_key) = std::env::var("ERUKA_SERVICE_KEY") {
            req = req.header("X-Service-Key", &service_key);
            req = req.header("X-Workspace-Id", user_id);
        } else if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Eruka API error {}: {}", status, body_text));
        }
        let json: serde_json::Value = response.json().await?;
        Ok(json)
    }

    /// Retrieves completeness for a user and scope.
    ///
    /// GET /api/v1/completeness/{scope}?user_id=...
    pub async fn get_completeness(&self, user_id: &str, scope: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v1/completeness/{}", self.base_url, scope);
        let mut req = self.http.get(&url).query(&[("user_id", user_id)]);
        if let Ok(service_key) = std::env::var("ERUKA_SERVICE_KEY") {
            req = req.header("X-Service-Key", &service_key);
            req = req.header("X-Workspace-Id", user_id);
        } else if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Eruka API error {}: {}", status, body_text));
        }
        let json: serde_json::Value = response.json().await?;
        Ok(json)
    }

    /// Links a tenant to a workspace.
    ///
    /// PUT /api/v1/workspaces/{workspace_id}/tenant
    pub async fn link_tenant(&self, workspace_id: &str, tenant_id: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v1/workspaces/{}/tenant", self.base_url, workspace_id);
        let body = serde_json::json!({"tenant_id": tenant_id});
        let mut req = self.http.put(&url).json(&body);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Eruka API error {}: {}", status, body_text));
        }
        let json: serde_json::Value = response.json().await?;
        Ok(json)
    }

    /// Batch import fields into a workspace and optionally build the tree.
    ///
    /// POST /api/v1/workspaces/{workspace_id}/import
    pub async fn import_workspace_fields(
        &self,
        workspace_id: &str,
        fields: &[serde_json::Value],
        build_tree: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v1/workspaces/{}/import", self.base_url, workspace_id);
        let body = serde_json::json!({
            "fields": fields,
            "build_tree": build_tree,
            "source": "survey"
        });
        let mut req = self.http.post(&url).json(&body);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        // Service-to-service auth
        if let Ok(service_key) = std::env::var("ERUKA_SERVICE_KEY") {
            req = req.header("X-Service-Key", service_key);
            req = req.header("X-Workspace-Id", workspace_id);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Eruka import error {}: {}", status, body_text));
        }
        let json: serde_json::Value = response.json().await?;
        Ok(json)
    }

    /// Build the tree for a workspace.
    ///
    /// POST /api/v1/tree/build (with service auth using workspace_id)
    pub async fn build_workspace_tree(&self, workspace_id: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v1/tree/build", self.base_url);
        let mut req = self.http.post(&url);
        if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        if let Ok(service_key) = std::env::var("ERUKA_SERVICE_KEY") {
            req = req.header("X-Service-Key", service_key);
            req = req.header("X-Workspace-Id", workspace_id);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Eruka tree build error {}: {}", status, body_text));
        }
        let json: serde_json::Value = response.json().await?;
        Ok(json)
    }

    /// Creates an Eruka user account (for DSprint activate).
    ///
    /// POST /api/v1/auth/signup — public endpoint, no auth needed
    pub async fn signup_user(&self, email: &str, password: &str, name: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v1/auth/signup", self.base_url);
        let body = serde_json::json!({"email": email, "password": password, "name": name});
        let response = self.http.post(&url).json(&body).send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Eruka signup error {}: {}", status, body_text));
        }
        let json: serde_json::Value = response.json().await?;
        Ok(json)
    }

    /// Retrieves a workspace by ID.
    ///
    /// GET /api/v1/workspaces/{workspace_id}
    pub async fn get_workspace(&self, workspace_id: &str) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/api/v1/workspaces/{}", self.base_url, workspace_id);
        let mut req = self.http.get(&url);
        if let Ok(service_key) = std::env::var("ERUKA_SERVICE_KEY") {
            req = req.header("X-Service-Key", &service_key);
            req = req.header("X-Workspace-Id", workspace_id);
        } else if let Some(auth) = self.auth_header() {
            req = req.header("Authorization", auth);
        }
        let response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Eruka API error {}: {}", status, body_text));
        }
        let json: serde_json::Value = response.json().await?;
        Ok(json)
    }
}
