use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub enabled: bool,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub timeout_secs: Option<u64>,
    pub endpoint: Option<String>,
    pub transport: Option<String>,
    pub api_key: Option<String>,
}

pub struct McpClient {
    config: McpServerConfig,
    http: reqwest::Client,
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("MCP server returned error: {0}")]
    ServerError(String),
    #[error("Tool not found: {0}")]
    ToolNotFound(String),
    #[error("Deserialize error: {0}")]
    Deserialize(#[from] serde_json::Error),
    #[error("MCP server is disabled")]
    ServerDisabled,
    #[error("No endpoint configured")]
    NoEndpoint,
}

impl McpClient {
    pub fn new(config: McpServerConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                config.timeout_secs.unwrap_or(30),
            ))
            .build()
            .expect("Failed to create HTTP client");

        Self { config, http }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub async fn get_context(&self, path: &str) -> Result<Value, McpError> {
        let base_url = self.get_base_url()?;
        let url = format!("{}/api/v1/context?path={}", base_url, path);
        
        let mut request = self.http.get(&url);
        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
        
        let response = request.send().await?;
        self.handle_response(response).await
    }

    pub async fn write_context(&self, path: &str, value: &str) -> Result<Value, McpError> {
        let base_url = self.get_base_url()?;
        let url = format!("{}/api/v1/context", base_url);
        
        let body = serde_json::json!({
            "path": path,
            "value": value
        });
        
        let mut request = self.http.post(&url).json(&body);
        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
        
        let response = request.send().await?;
        self.handle_response(response).await
    }

    pub async fn search_context(&self, query: &str, scope: Option<&str>, max_results: Option<usize>) -> Result<Value, McpError> {
        let base_url = self.get_base_url()?;
        let url = format!("{}/api/v1/context/search", base_url);
        
        let mut body = serde_json::json!({
            "query": query
        });
        if let Some(s) = scope {
            body["scope"] = serde_json::json!(s);
        }
        if let Some(m) = max_results {
            body["max_results"] = serde_json::json!(m);
        }
        
        let mut request = self.http.post(&url).json(&body);
        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
        
        let response = request.send().await?;
        self.handle_response(response).await
    }

    pub async fn get_completeness(&self, scope: Option<&str>) -> Result<Value, McpError> {
        let base_url = self.get_base_url()?;
        let scope_part = scope.unwrap_or("*");
        let url = format!("{}/api/v1/completeness/{}", base_url, scope_part);
        
        let mut request = self.http.get(&url);
        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
        
        let response = request.send().await?;
        self.handle_response(response).await
    }

    pub async fn get_gaps(&self, status: Option<&str>, category: Option<&str>) -> Result<Value, McpError> {
        let base_url = self.get_base_url()?;
        let url = format!("{}/api/v1/gaps", base_url);
        
        let mut request = self.http.get(&url);
        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
        
        let response = request.send().await?;
        self.handle_response(response).await
    }

    pub async fn detect_gaps(&self, category: Option<&str>) -> Result<Value, McpError> {
        let base_url = self.get_base_url()?;
        let url = format!("{}/api/v1/gaps/detect", base_url);
        
        let body = if let Some(cat) = category {
            serde_json::json!({ "category": cat })
        } else {
            serde_json::json!({})
        };
        
        let mut request = self.http.post(&url).json(&body);
        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }
        
        let response = request.send().await?;
        self.handle_response(response).await
    }

    fn get_base_url(&self) -> Result<String, McpError> {
        self.config
            .endpoint
            .clone()
            .ok_or(McpError::NoEndpoint)
    }

    async fn handle_response(&self, response: reqwest::Response) -> Result<Value, McpError> {
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(McpError::ServerError(format!("HTTP {}: {}", status, text)));
        }

        let result: Value = response.json().await?;
        Ok(result)
    }
}
