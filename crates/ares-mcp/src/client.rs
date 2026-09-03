use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// Default HTTP client timeout when config omits `timeout_secs`.
pub const DEFAULT_CLIENT_TIMEOUT_SECS: u64 = 30;

/// Maximum automatic retries for retryable HTTP failures and timeouts.
pub const DEFAULT_MAX_RETRIES: u32 = 3;

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

/// Serializable description of an outbound MCP HTTP call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpClientRequest {
    pub method: McpHttpMethod,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<Vec<(String, String)>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// Materialized HTTP request (URL, headers, body) ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltMcpHttpRequest {
    pub url: String,
    pub method: McpHttpMethod,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Value>,
}

/// Parsed successful MCP HTTP payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpClientResponse {
    pub status: u16,
    pub data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum McpHttpMethod {
    Get,
    Post,
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

/// Resolve the effective per-request timeout from server config.
pub fn client_timeout_secs(timeout_secs: Option<u64>) -> u64 {
    timeout_secs.unwrap_or(DEFAULT_CLIENT_TIMEOUT_SECS)
}

/// Build a fully qualified URL, headers, and JSON body for an MCP HTTP request.
pub fn build_client_request(
    base_url: &str,
    request: &McpClientRequest,
    api_key: Option<&str>,
) -> Result<BuiltMcpHttpRequest, McpError> {
    let path = request.path.trim();
    if path.is_empty() {
        return Err(McpError::ServerError("empty request path".into()));
    }

    let relative = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };

    let mut url = format!("{base_url}{relative}");
    if let Some(query) = &request.query {
        if !query.is_empty() {
            let qs: Vec<String> = query
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
                .collect();
            url.push('?');
            url.push_str(&qs.join("&"));
        }
    }

    let mut headers = BTreeMap::new();
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        headers.insert("Authorization".into(), format!("Bearer {key}"));
    }
    if request.body.is_some() {
        headers.insert("Content-Type".into(), "application/json".into());
    }

    Ok(BuiltMcpHttpRequest {
        url,
        method: request.method,
        headers,
        body: request.body.clone(),
    })
}

/// Parse an HTTP status code and response body into JSON or a mapped error.
pub fn parse_client_response(status: u16, body_text: &str) -> Result<Value, McpError> {
    if !is_success_status(status) {
        return Err(map_http_status_error(status, body_text));
    }

    if body_text.trim().is_empty() {
        return Ok(Value::Null);
    }

    serde_json::from_str(body_text).map_err(McpError::from)
}

/// Map a non-success HTTP status and body to [`McpError::ServerError`].
pub fn map_http_status_error(status: u16, body: &str) -> McpError {
    McpError::ServerError(format!("HTTP {status}: {body}"))
}

/// Extract the HTTP status code embedded in [`McpError::ServerError`], if present.
pub fn http_status_from_error(err: &McpError) -> Option<u16> {
    let McpError::ServerError(message) = err else {
        return None;
    };
    let rest = message.strip_prefix("HTTP ")?;
    let status_str = rest.split(':').next()?.trim();
    status_str.parse().ok()
}

/// Whether an HTTP status should trigger another attempt.
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// Whether a transport-level failure (timeout, connect) should retry.
pub fn is_retryable_transport(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect()
}

/// Decide if another attempt is warranted (pure simulation helper).
pub fn should_retry_request(
    attempt: u32,
    max_attempts: u32,
    status: Option<u16>,
    transport_retryable: bool,
) -> bool {
    if attempt >= max_attempts {
        return false;
    }
    if transport_retryable {
        return true;
    }
    status.is_some_and(is_retryable_status)
}

/// Exponential backoff delay in milliseconds for a 1-based attempt index.
pub fn retry_backoff_ms(attempt: u32, base_ms: u64) -> u64 {
    base_ms.saturating_mul(1u64 << attempt.saturating_sub(1).min(8))
}

fn is_success_status(status: u16) -> bool {
    (200..300).contains(&status)
}

fn urlencoding_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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
        return std::env::var(name).map_err(|_| McpConfigError::MissingEnvVar(name.to_string()));
    }
    if let Some(name) = trimmed.strip_prefix('$').filter(|n| !n.is_empty()) {
        return std::env::var(name).map_err(|_| McpConfigError::MissingEnvVar(name.to_string()));
    }
    Ok(trimmed.to_string())
}

/// Apply endpoint normalization and env-placeholder resolution to a parsed config.
pub fn normalize_mcp_config(
    mut config: McpServerConfig,
) -> Result<McpServerConfig, McpConfigError> {
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
        Err(toml_error) => {
            toon_format::decode_default::<McpServerConfig>(content).map_err(|toon_error| {
                McpConfigError::Parse(format!(
                    "failed to parse as TOML ({toml_error}) or TOON ({toon_error})"
                ))
            })?
        }
    };
    normalize_mcp_config(config)
}

impl McpClient {
    pub fn new(config: McpServerConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(client_timeout_secs(
                config.timeout_secs,
            )))
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
        self.execute(McpClientRequest {
            method: McpHttpMethod::Get,
            path: "/api/v1/context".into(),
            query: Some(vec![("path".into(), path.into())]),
            body: None,
        })
        .await
    }

    pub async fn write_context(&self, path: &str, value: &str) -> Result<Value, McpError> {
        self.execute(McpClientRequest {
            method: McpHttpMethod::Post,
            path: "/api/v1/context".into(),
            query: None,
            body: Some(serde_json::json!({
                "path": path,
                "value": value
            })),
        })
        .await
    }

    pub async fn search_context(
        &self,
        query: &str,
        scope: Option<&str>,
        max_results: Option<usize>,
    ) -> Result<Value, McpError> {
        let mut body = serde_json::json!({ "query": query });
        if let Some(s) = scope {
            body["scope"] = serde_json::json!(s);
        }
        if let Some(m) = max_results {
            body["max_results"] = serde_json::json!(m);
        }

        self.execute(McpClientRequest {
            method: McpHttpMethod::Post,
            path: "/api/v1/context/search".into(),
            query: None,
            body: Some(body),
        })
        .await
    }

    pub async fn get_completeness(&self, scope: Option<&str>) -> Result<Value, McpError> {
        let scope_part = scope.unwrap_or("*");
        self.execute(McpClientRequest {
            method: McpHttpMethod::Get,
            path: format!("/api/v1/completeness/{scope_part}"),
            query: None,
            body: None,
        })
        .await
    }

    pub async fn get_gaps(
        &self,
        status: Option<&str>,
        category: Option<&str>,
    ) -> Result<Value, McpError> {
        let mut query = Vec::new();
        if let Some(s) = status {
            query.push(("status".into(), s.into()));
        }
        if let Some(c) = category {
            query.push(("category".into(), c.into()));
        }

        self.execute(McpClientRequest {
            method: McpHttpMethod::Get,
            path: "/api/v1/gaps".into(),
            query: if query.is_empty() { None } else { Some(query) },
            body: None,
        })
        .await
    }

    pub async fn detect_gaps(&self, category: Option<&str>) -> Result<Value, McpError> {
        let body = if let Some(cat) = category {
            serde_json::json!({ "category": cat })
        } else {
            serde_json::json!({})
        };

        self.execute(McpClientRequest {
            method: McpHttpMethod::Post,
            path: "/api/v1/gaps/detect".into(),
            query: None,
            body: Some(body),
        })
        .await
    }

    async fn execute(&self, request: McpClientRequest) -> Result<Value, McpError> {
        if !self.config.enabled {
            return Err(McpError::ServerDisabled);
        }

        let base_url = self.get_base_url()?;
        let built = build_client_request(&base_url, &request, self.config.api_key.as_deref())?;

        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.send_built(&built).await {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let status = http_status_from_error(&err);
                    let transport_retryable =
                        matches!(&err, McpError::Http(e) if is_retryable_transport(e));
                    if should_retry_request(
                        attempt,
                        DEFAULT_MAX_RETRIES,
                        status,
                        transport_retryable,
                    ) {
                        tokio::time::sleep(Duration::from_millis(retry_backoff_ms(attempt, 100)))
                            .await;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    async fn send_built(&self, built: &BuiltMcpHttpRequest) -> Result<Value, McpError> {
        let mut request = match built.method {
            McpHttpMethod::Get => self.http.get(&built.url),
            McpHttpMethod::Post => self.http.post(&built.url),
        };

        for (name, value) in &built.headers {
            request = request.header(name.as_str(), value.as_str());
        }

        let response = if let Some(body) = &built.body {
            request.json(body).send().await?
        } else {
            request.send().await?
        };

        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        parse_client_response(status, &text)
    }

    fn get_base_url(&self) -> Result<String, McpError> {
        let endpoint = self
            .config
            .endpoint
            .as_deref()
            .ok_or(McpError::NoEndpoint)?;
        normalize_endpoint_url(endpoint).map_err(McpError::from)
    }
}

#[cfg(test)]
impl McpClient {
    fn base_url_for_test(&self) -> Result<String, McpError> {
        self.get_base_url()
    }

    fn build_for_test(&self, request: &McpClientRequest) -> Result<BuiltMcpHttpRequest, McpError> {
        let base_url = self.get_base_url()?;
        build_client_request(&base_url, request, self.config.api_key.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    fn client_for_server(server: &MockServer) -> McpClient {
        McpClient::new(McpServerConfig {
            endpoint: Some(server.uri()),
            api_key: Some("test-key".into()),
            ..sample_config()
        })
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
        assert_eq!(McpError::NoEndpoint.to_string(), "No endpoint configured");
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

    #[test]
    fn mcp_client_request_serde_roundtrip_get_with_query() {
        let request = McpClientRequest {
            method: McpHttpMethod::Get,
            path: "/api/v1/context".into(),
            query: Some(vec![("path".into(), "docs/readme".into())]),
            body: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"GET\""));
        let restored: McpClientRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, request);
    }

    #[test]
    fn mcp_client_request_serde_roundtrip_post_with_body() {
        let request = McpClientRequest {
            method: McpHttpMethod::Post,
            path: "/api/v1/context".into(),
            query: None,
            body: Some(serde_json::json!({"path": "a", "value": "b"})),
        };
        let restored: McpClientRequest =
            serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
        assert_eq!(restored.method, McpHttpMethod::Post);
        assert_eq!(restored.body, request.body);
    }

    #[test]
    fn mcp_client_response_serde_roundtrip() {
        let response = McpClientResponse {
            status: 200,
            data: serde_json::json!({"ok": true}),
        };
        let restored: McpClientResponse =
            serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
        assert_eq!(restored, response);
    }

    #[test]
    fn mcp_http_method_serde_uses_uppercase() {
        let method = serde_json::from_str::<McpHttpMethod>("\"POST\"").unwrap();
        assert_eq!(method, McpHttpMethod::Post);
        assert_eq!(
            serde_json::to_string(&McpHttpMethod::Get).unwrap(),
            "\"GET\""
        );
    }

    #[test]
    fn build_client_request_get_url_query_and_auth() {
        let built = build_client_request(
            "https://api.test.com/mcp",
            &McpClientRequest {
                method: McpHttpMethod::Get,
                path: "/api/v1/context".into(),
                query: Some(vec![("path".into(), "docs/read me".into())]),
                body: None,
            },
            Some("tok"),
        )
        .unwrap();

        assert_eq!(
            built.url,
            "https://api.test.com/mcp/api/v1/context?path=docs%2Fread%20me"
        );
        assert_eq!(
            built.headers.get("Authorization").map(String::as_str),
            Some("Bearer tok")
        );
    }

    #[test]
    fn build_client_request_post_sets_json_content_type() {
        let body = serde_json::json!({"query": "rust"});
        let built = build_client_request(
            "https://api.test.com",
            &McpClientRequest {
                method: McpHttpMethod::Post,
                path: "api/v1/context/search".into(),
                query: None,
                body: Some(body.clone()),
            },
            None,
        )
        .unwrap();

        assert_eq!(built.url, "https://api.test.com/api/v1/context/search");
        assert_eq!(built.body, Some(body));
        assert_eq!(
            built.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );
    }

    #[test]
    fn build_client_request_rejects_empty_path() {
        let err = build_client_request(
            "https://api.test.com",
            &McpClientRequest {
                method: McpHttpMethod::Get,
                path: "   ".into(),
                query: None,
                body: None,
            },
            None,
        )
        .unwrap_err();
        assert!(matches!(err, McpError::ServerError(_)));
    }

    #[test]
    fn build_client_request_via_client_helper() {
        let client = McpClient::new(sample_config());
        let built = client
            .build_for_test(&McpClientRequest {
                method: McpHttpMethod::Get,
                path: "/api/v1/gaps".into(),
                query: Some(vec![("status".into(), "open".into())]),
                body: None,
            })
            .unwrap();
        assert!(built.url.starts_with("https://example.com/mcp/api/v1/gaps"));
        assert_eq!(
            built.headers.get("Authorization").map(String::as_str),
            Some("Bearer secret")
        );
    }

    #[test]
    fn parse_client_response_success_json() {
        let value = parse_client_response(200, r#"{"items":[1]}"#).unwrap();
        assert_eq!(value["items"][0], 1);
    }

    #[test]
    fn parse_client_response_success_empty_body_is_null() {
        assert_eq!(parse_client_response(204, "").unwrap(), Value::Null);
    }

    #[test]
    fn parse_client_response_error_status_maps_message() {
        let err = parse_client_response(404, "missing").unwrap_err();
        assert_eq!(
            err.to_string(),
            "MCP server returned error: HTTP 404: missing"
        );
    }

    #[test]
    fn parse_client_response_invalid_json_on_success_status() {
        assert!(matches!(
            parse_client_response(200, "not-json").unwrap_err(),
            McpError::Deserialize(_)
        ));
    }

    #[test]
    fn map_http_status_error_formats_status_and_body() {
        let err = map_http_status_error(503, "overloaded");
        assert_eq!(
            err.to_string(),
            "MCP server returned error: HTTP 503: overloaded"
        );
    }

    #[test]
    fn http_status_from_error_extracts_code() {
        let err = McpError::ServerError("HTTP 429: rate limited".into());
        assert_eq!(http_status_from_error(&err), Some(429));
        assert_eq!(http_status_from_error(&McpError::NoEndpoint), None);
    }

    #[test]
    fn client_timeout_secs_defaults_when_unset() {
        assert_eq!(client_timeout_secs(None), DEFAULT_CLIENT_TIMEOUT_SECS);
        assert_eq!(client_timeout_secs(Some(5)), 5);
    }

    #[test]
    fn is_retryable_status_matches_transient_codes() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(503));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(404));
    }

    #[test]
    fn should_retry_request_respects_attempt_budget() {
        assert!(should_retry_request(1, 3, Some(503), false));
        assert!(!should_retry_request(3, 3, Some(503), false));
        assert!(!should_retry_request(1, 3, Some(404), false));
        assert!(should_retry_request(2, 3, None, true));
    }

    #[test]
    fn retry_backoff_ms_grows_exponentially() {
        assert_eq!(retry_backoff_ms(1, 100), 100);
        assert_eq!(retry_backoff_ms(2, 100), 200);
        assert_eq!(retry_backoff_ms(3, 100), 400);
    }

    #[test]
    fn retry_simulation_recovers_after_transient_failures() {
        let statuses = [503_u16, 503, 200];
        let mut attempt = 0u32;
        let mut last: Option<Result<Value, McpError>> = None;
        for status in statuses {
            attempt += 1;
            let result = parse_client_response(status, r#"{"ok":true}"#);
            if result.is_ok() {
                last = Some(result);
                break;
            }
            let err = result.unwrap_err();
            if should_retry_request(attempt, 3, http_status_from_error(&err), false) {
                continue;
            }
            last = Some(Err(err));
            break;
        }
        assert_eq!(last.unwrap().unwrap()["ok"], true);
        assert_eq!(attempt, 3);
    }

    #[tokio::test]
    async fn wiremock_get_context_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/context"))
            .and(query_param("path", "docs/readme"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"path": "docs/readme"})),
            )
            .mount(&server)
            .await;

        let client = client_for_server(&server);
        let value = client.get_context("docs/readme").await.unwrap();
        assert_eq!(value["path"], "docs/readme");
    }

    #[tokio::test]
    async fn wiremock_write_context_posts_json_body() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({"path": "k", "value": "v"});
        Mock::given(method("POST"))
            .and(path("/api/v1/context"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_json(expected.clone()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"written": true})),
            )
            .mount(&server)
            .await;

        let client = client_for_server(&server);
        let value = client.write_context("k", "v").await.unwrap();
        assert_eq!(value["written"], true);
    }

    #[tokio::test]
    async fn wiremock_search_context_optional_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/context/search"))
            .and(body_json(serde_json::json!({
                "query": "ares",
                "scope": "docs",
                "max_results": 5
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"hits": 2})))
            .mount(&server)
            .await;

        let client = client_for_server(&server);
        let value = client
            .search_context("ares", Some("docs"), Some(5))
            .await
            .unwrap();
        assert_eq!(value["hits"], 2);
    }

    #[tokio::test]
    async fn wiremock_get_completeness_scope_in_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/completeness/docs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"score": 0.9})),
            )
            .mount(&server)
            .await;

        let client = client_for_server(&server);
        let value = client.get_completeness(Some("docs")).await.unwrap();
        assert_eq!(value["score"], 0.9);
    }

    #[tokio::test]
    async fn wiremock_get_gaps_query_params() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/gaps"))
            .and(query_param("status", "open"))
            .and(query_param("category", "docs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"gaps": []})))
            .mount(&server)
            .await;

        let client = client_for_server(&server);
        let value = client.get_gaps(Some("open"), Some("docs")).await.unwrap();
        assert!(value["gaps"].is_array());
    }

    #[tokio::test]
    async fn wiremock_detect_gaps_empty_category_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/gaps/detect"))
            .and(body_json(serde_json::json!({})))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"detected": 0})),
            )
            .mount(&server)
            .await;

        let client = client_for_server(&server);
        let value = client.detect_gaps(None).await.unwrap();
        assert_eq!(value["detected"], 0);
    }

    #[tokio::test]
    async fn wiremock_server_error_surfaces_without_retry_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/context"))
            .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
            .mount(&server)
            .await;

        let client = client_for_server(&server);
        let err = client.get_context("x").await.unwrap_err();
        assert!(err.to_string().contains("HTTP 404"));
    }

    #[tokio::test]
    async fn wiremock_retries_transient_503_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/context"))
            .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/context"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let client = client_for_server(&server);
        let value = client.get_context("x").await.unwrap();
        assert_eq!(value["ok"], true);
    }

    #[tokio::test]
    async fn wiremock_disabled_client_returns_server_disabled() {
        let server = MockServer::start().await;
        let client = McpClient::new(McpServerConfig {
            enabled: false,
            endpoint: Some(server.uri()),
            ..sample_config()
        });
        let err = client.get_context("x").await.unwrap_err();
        assert!(matches!(err, McpError::ServerDisabled));
    }
}
