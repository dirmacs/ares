use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpConfigError {
    #[error("empty endpoint URL")]
    EmptyEndpoint,
    #[error("failed to parse config: {0}")]
    Parse(String),
    #[error("environment variable not set: {0}")]
    MissingEnvVar(String),
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
    #[error("Invalid MCP configuration: {0}")]
    Config(#[from] McpConfigError),
}

/// Normalize an MCP HTTP endpoint URL for use as a request base URL.
///
/// Trims surrounding whitespace, prepends `https://` when no scheme is present,
/// and strips trailing slashes so path joins stay stable.
pub fn normalize_endpoint_url(raw: &str) -> Result<String, McpConfigError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(McpConfigError::EmptyEndpoint);
    }

    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    Ok(with_scheme.trim_end_matches('/').to_string())
}

/// Resolve `$VAR` or `${VAR}` placeholders from the process environment.
///
/// Literal values (no leading `$`) are returned unchanged.
pub fn resolve_env_value(value: &str) -> Result<String, McpConfigError> {
    let trimmed = value.trim();
    if let Some(name) = trimmed
        .strip_prefix("${")
        .and_then(|inner| inner.strip_suffix('}'))
    {
        return std::env::var(name)
            .map_err(|_| McpConfigError::MissingEnvVar(name.to_string()));
    }
    if let Some(name) = trimmed.strip_prefix('$').filter(|n| !n.is_empty()) {
        return std::env::var(name)
            .map_err(|_| McpConfigError::MissingEnvVar(name.to_string()));
    }
    Ok(trimmed.to_string())
}

/// Apply endpoint normalization and env-placeholder resolution to a parsed config.
pub fn normalize_mcp_config(mut config: McpServerConfig) -> Result<McpServerConfig, McpConfigError> {
    if let Some(endpoint) = config.endpoint.take() {
        config.endpoint = Some(normalize_endpoint_url(&endpoint)?);
    }
    if let Some(api_key) = config.api_key.take() {
        config.api_key = Some(resolve_env_value(&api_key)?);
    }
    Ok(config)
}

/// Parse MCP server config from TOML or TOON text and normalize fields.
pub fn parse_mcp_config(content: &str) -> Result<McpServerConfig, McpConfigError> {
    let config = match toml::from_str::<McpServerConfig>(content) {
        Ok(config) => config,
        Err(toml_error) => toon_format::decode_default::<McpServerConfig>(content).map_err(|toon_error| {
            McpConfigError::Parse(format!(
                "failed to parse as TOML ({toml_error}) or TOON ({toon_error})"
            ))
        })?,
    };
    normalize_mcp_config(config)
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

    pub async fn search_context(
        &self,
        query: &str,
        scope: Option<&str>,
        max_results: Option<usize>,
    ) -> Result<Value, McpError> {
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

    pub async fn get_gaps(
        &self,
        status: Option<&str>,
        category: Option<&str>,
    ) -> Result<Value, McpError> {
        let base_url = self.get_base_url()?;
        let url = format!("{}/api/v1/gaps", base_url);

        let mut request = self.http.get(&url);
        if let Some(ref key) = self.config.api_key {
            request = request.header("Authorization", format!("Bearer {}", key));
        }

        if let Some(s) = status {
            request = request.query(&[("status", s)]);
        }
        if let Some(c) = category {
            request = request.query(&[("category", c)]);
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
        let endpoint = self.config.endpoint.as_deref().ok_or(McpError::NoEndpoint)?;
        normalize_endpoint_url(endpoint).map_err(McpError::from)
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

#[cfg(test)]
impl McpClient {
    fn base_url_for_test(&self) -> Result<String, McpError> {
        self.get_base_url()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MISSING_ENV: &str = "ARES_MCP_TEST_MISSING_API_KEY_7f3a";

    fn sample_config() -> McpServerConfig {
        McpServerConfig {
            name: "eruka".into(),
            enabled: true,
            command: None,
            args: None,
            timeout_secs: Some(30),
            endpoint: Some("https://example.com/mcp".into()),
            transport: Some("http".into()),
            api_key: Some("secret".into()),
        }
    }

    #[test]
    fn normalize_endpoint_url_trims_whitespace() {
        assert_eq!(
            normalize_endpoint_url("  https://example.com/mcp/  ").unwrap(),
            "https://example.com/mcp"
        );
    }

    #[test]
    fn normalize_endpoint_url_adds_https_scheme() {
        assert_eq!(
            normalize_endpoint_url("localhost:3002/mcp").unwrap(),
            "https://localhost:3002/mcp"
        );
    }

    #[test]
    fn normalize_endpoint_url_preserves_explicit_scheme() {
        assert_eq!(
            normalize_endpoint_url("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn normalize_endpoint_url_strips_multiple_trailing_slashes() {
        assert_eq!(
            normalize_endpoint_url("https://api.test.com///").unwrap(),
            "https://api.test.com"
        );
    }

    #[test]
    fn normalize_endpoint_url_rejects_empty() {
        assert_eq!(
            normalize_endpoint_url("").unwrap_err(),
            McpConfigError::EmptyEndpoint
        );
        assert_eq!(
            normalize_endpoint_url("   ").unwrap_err(),
            McpConfigError::EmptyEndpoint
        );
    }

    #[test]
    fn parse_mcp_config_toml_normalizes_endpoint() {
        let config = parse_mcp_config(
            r#"
name = "eruka"
enabled = true
endpoint = "https://eruka.example.com/mcp/"
transport = "http"
timeout_secs = 30
"#,
        )
        .unwrap();

        assert_eq!(config.name, "eruka");
        assert_eq!(
            config.endpoint.as_deref(),
            Some("https://eruka.example.com/mcp")
        );
    }

    #[test]
    fn parse_mcp_config_toon_parses_command_server() {
        let config = parse_mcp_config(
            r#"name: filesystem
enabled: true
command: npx
args[2]: "-y","@modelcontextprotocol/server-filesystem"
timeout_secs: 30
"#,
        )
        .unwrap();

        assert_eq!(config.name, "filesystem");
        assert_eq!(config.command.as_deref(), Some("npx"));
        let args = config.args.expect("args");
        assert_eq!(args[0], "-y");
        assert_eq!(args[1], "@modelcontextprotocol/server-filesystem");
        assert!(config.endpoint.is_none());
    }

    #[test]
    fn parse_mcp_config_invalid_content_returns_parse_error() {
        let err = parse_mcp_config("not valid =").unwrap_err();
        assert!(matches!(err, McpConfigError::Parse(_)));
        assert!(err.to_string().contains("failed to parse"));
    }

    #[test]
    fn resolve_env_value_returns_literal_secret() {
        assert_eq!(resolve_env_value("plain-secret").unwrap(), "plain-secret");
    }

    #[test]
    fn resolve_env_value_reads_existing_var() {
        if std::env::var("PATH").is_err() {
            return;
        }
        let resolved = resolve_env_value("$PATH").unwrap();
        assert_eq!(resolved, std::env::var("PATH").unwrap());
    }

    #[test]
    fn resolve_env_value_missing_var_errors() {
        std::env::remove_var(MISSING_ENV);
        let err = resolve_env_value(&format!("${{{MISSING_ENV}}}")).unwrap_err();
        assert_eq!(err, McpConfigError::MissingEnvVar(MISSING_ENV.to_string()));
    }

    #[test]
    fn normalize_mcp_config_resolves_api_key_placeholder() {
        std::env::remove_var(MISSING_ENV);
        let err = normalize_mcp_config(McpServerConfig {
            api_key: Some(format!("${{{MISSING_ENV}}}")),
            ..sample_config()
        })
        .unwrap_err();
        assert_eq!(err, McpConfigError::MissingEnvVar(MISSING_ENV.to_string()));
    }

    #[test]
    fn mcp_server_config_round_trips_json() {
        let config = sample_config();
        let json = serde_json::to_string(&config).unwrap();
        let restored: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, config);
    }

    #[test]
    fn mcp_server_config_round_trips_toml() {
        let config = sample_config();
        let toml_text = toml::to_string(&config).unwrap();
        let restored: McpServerConfig = toml::from_str(&toml_text).unwrap();
        assert_eq!(restored.name, config.name);
        assert_eq!(restored.endpoint, config.endpoint);
        assert_eq!(restored.api_key, config.api_key);
    }

    #[test]
    fn mcp_server_config_round_trips_toon() {
        let config = sample_config();
        let toon_text = toon_format::encode_default(&config).unwrap();
        let restored: McpServerConfig = toon_format::decode_default(&toon_text).unwrap();
        assert_eq!(restored.name, config.name);
        assert_eq!(restored.enabled, config.enabled);
        assert_eq!(restored.transport, config.transport);
    }

    #[test]
    fn mcp_error_display_messages() {
        assert_eq!(
            McpError::NoEndpoint.to_string(),
            "No endpoint configured"
        );
        assert_eq!(
            McpError::ServerDisabled.to_string(),
            "MCP server is disabled"
        );
        assert_eq!(
            McpError::ToolNotFound("search".into()).to_string(),
            "Tool not found: search"
        );
        assert_eq!(
            McpError::Config(McpConfigError::EmptyEndpoint).to_string(),
            "Invalid MCP configuration: empty endpoint URL"
        );
    }

    #[test]
    fn mcp_client_base_url_requires_endpoint() {
        let client = McpClient::new(McpServerConfig {
            endpoint: None,
            ..sample_config()
        });
        assert!(matches!(
            client.base_url_for_test(),
            Err(McpError::NoEndpoint)
        ));
    }

    #[test]
    fn mcp_client_base_url_normalizes_trailing_slash() {
        let client = McpClient::new(McpServerConfig {
            endpoint: Some("https://api.test.com/mcp/".into()),
            ..sample_config()
        });
        assert_eq!(
            client.base_url_for_test().unwrap(),
            "https://api.test.com/mcp"
        );
    }

    #[test]
    fn mcp_client_exposes_config_name() {
        let client = McpClient::new(McpServerConfig {
            name: "filesystem".into(),
            enabled: true,
            command: Some("npx".into()),
            args: Some(vec!["-y".into()]),
            timeout_secs: None,
            endpoint: None,
            transport: None,
            api_key: None,
        });
        assert_eq!(client.name(), "filesystem");
        assert!(client.is_enabled());
    }
}
