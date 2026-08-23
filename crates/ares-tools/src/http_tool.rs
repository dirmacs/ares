//! Runtime HTTP tool executor for A.R.E.S.
//!
//! This tool makes HTTP requests using templates configured via `execution_config`.
//! Templates use `{{param_name}}` syntax for parameter substitution from tool
//! arguments.

use crate::registry::Tool;
use ares_types::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

// =============================================================================
// Configuration
// =============================================================================

/// HTTP-specific configuration parsed from `execution_config` JSONB.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HttpToolConfig {
    /// HTTP method: GET, POST, PUT, PATCH, DELETE, etc.
    pub method: String,
    /// URL template with `{{param}}` placeholders.
    pub url_template: String,
    /// Optional header templates (object with `{{param}}` values).
    #[serde(default)]
    pub headers_template: Option<Value>,
    /// Optional body template (JSON value with `{{param}}` placeholders).
    #[serde(default)]
    pub body_template: Option<Value>,
    /// Request timeout in seconds (default: 30).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl Default for HttpToolConfig {
    fn default() -> Self {
        Self {
            method: "GET".to_string(),
            url_template: String::new(),
            headers_template: None,
            body_template: None,
            timeout_secs: Some(30),
        }
    }
}

// =============================================================================
// Tool implementation
// =============================================================================

/// Runtime HTTP tool that executes templated HTTP requests.
pub struct HttpTool {
    name: String,
    description: String,
    parameters_schema: Value,
    config: HttpToolConfig,
    client: reqwest::Client,
}

impl HttpTool {
    /// Create an HTTP tool from its runtime configuration.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: Value,
        config: HttpToolConfig,
    ) -> Self {
        let timeout = std::time::Duration::from_secs(config.timeout_secs.unwrap_or(30));
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();

        Self {
            name: name.into(),
            description: description.into(),
            parameters_schema,
            config,
            client,
        }
    }

    /// Parse execution_config JSONB into [`HttpToolConfig`].
    pub fn parse_config(execution_config: &Value) -> Result<HttpToolConfig> {
        serde_json::from_value(execution_config.clone()).map_err(|e| {
            ares_types::AppError::Configuration(format!("Invalid HTTP tool config: {e}"))
        })
    }
}

#[async_trait]
impl Tool for HttpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters_schema.clone()
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let args_map = args.as_object().ok_or_else(|| {
            ares_types::AppError::InvalidInput("args must be a JSON object".to_string())
        })?;

        // --- Build URL ---
        let url = substitute_template_string(&self.config.url_template, args_map)?;
        if url.is_empty() {
            return Err(ares_types::AppError::InvalidInput(
                "url_template resolved to an empty string".to_string(),
            ));
        }

        // --- Parse method ---
        let method = parse_http_method(&self.config.method)?;

        // --- Build request ---
        let mut request = self.client.request(method, &url);

        // --- Substitute and attach headers ---
        if let Some(headers) = &self.config.headers_template {
            let substituted = substitute_template_value(headers, args_map)?;
            if let Some(obj) = substituted.as_object() {
                for (key, value) in obj {
                    let header_value = value.as_str().ok_or_else(|| {
                        ares_types::AppError::InvalidInput(format!(
                            "header '{key}' must resolve to a string"
                        ))
                    })?;
                    request = request.header(key, header_value);
                }
            }
        }

        // --- Substitute and attach body ---
        if let Some(body) = &self.config.body_template {
            let substituted = substitute_template_value(body, args_map)?;
            request = request.json(&substituted);
        }

        // --- Execute ---
        let response = request
            .send()
            .await
            .map_err(|e| ares_types::AppError::External(format!("HTTP request failed: {e}")))?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| {
                let val = v.to_str().unwrap_or("").to_string();
                (k.to_string(), val)
            })
            .collect::<HashMap<String, String>>();

        // Attempt to parse body as JSON, fall back to text
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let body_value = if content_type.contains("application/json") {
            response.json::<Value>().await.unwrap_or(Value::Null)
        } else {
            let text = response.text().await.unwrap_or_default();
            json!(text)
        };

        Ok(json!({
            "status": status,
            "headers": headers,
            "body": body_value,
        }))
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Replace `{{key}}` placeholders in `template` with values from `args`.
fn substitute_template_string(
    template: &str,
    args: &serde_json::Map<String, Value>,
) -> Result<String> {
    let mut result = template.to_string();
    for (key, value) in args {
        let placeholder = format!("{{{{{}}}}}", key);
        let replacement = value_to_string(value)?;
        result = result.replace(&placeholder, &replacement);
    }
    Ok(result)
}

/// Recursively replace `{{key}}` placeholders inside a JSON value.
fn substitute_template_value(
    value: &Value,
    args: &serde_json::Map<String, Value>,
) -> Result<Value> {
    match value {
        Value::String(s) => Ok(Value::String(substitute_template_string(s, args)?)),
        Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (k, v) in map {
                new_map.insert(k.clone(), substitute_template_value(v, args)?);
            }
            Ok(Value::Object(new_map))
        }
        Value::Array(arr) => {
            let new_arr = arr
                .iter()
                .map(|v| substitute_template_value(v, args))
                .collect::<Result<Vec<_>>>()?;
            Ok(Value::Array(new_arr))
        }
        other => Ok(other.clone()),
    }
}

/// Convert a JSON value to its string representation for URL/header/body substitution.
fn value_to_string(value: &Value) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Null => Ok(String::new()),
        _ => Err(ares_types::AppError::InvalidInput(
            "Template substitution does not support objects or arrays as scalar replacements"
                .to_string(),
        )),
    }
}

/// Parse a method string into a `reqwest::Method`.
fn parse_http_method(method: &str) -> Result<reqwest::Method> {
    match method.to_ascii_uppercase().as_str() {
        "GET" => Ok(reqwest::Method::GET),
        "POST" => Ok(reqwest::Method::POST),
        "PUT" => Ok(reqwest::Method::PUT),
        "PATCH" => Ok(reqwest::Method::PATCH),
        "DELETE" => Ok(reqwest::Method::DELETE),
        "HEAD" => Ok(reqwest::Method::HEAD),
        "OPTIONS" => Ok(reqwest::Method::OPTIONS),
        "TRACE" => Ok(reqwest::Method::TRACE),
        _ => Err(ares_types::AppError::InvalidInput(format!(
            "Unsupported HTTP method: {method}"
        ))),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_test_tool(config: HttpToolConfig) -> HttpTool {
        HttpTool::new(
            "test_http",
            "A test HTTP tool",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                }
            }),
            config,
        )
    }

    #[test]
    fn test_name_and_description() {
        let tool = make_test_tool(HttpToolConfig::default());
        assert_eq!(tool.name(), "test_http");
        assert_eq!(tool.description(), "A test HTTP tool");
    }

    #[test]
    fn test_parameters_schema() {
        let tool = make_test_tool(HttpToolConfig::default());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn test_parse_config() {
        let raw = json!({
            "method": "POST",
            "url_template": "https://api.example.com/{{endpoint}}",
            "headers_template": { "Authorization": "Bearer {{token}}" },
            "body_template": { "query": "{{query}}" },
            "timeout_secs": 15
        });
        let cfg = HttpTool::parse_config(&raw).unwrap();
        assert_eq!(cfg.method, "POST");
        assert_eq!(cfg.url_template, "https://api.example.com/{{endpoint}}");
        assert_eq!(cfg.timeout_secs, Some(15));
    }

    #[test]
    fn test_parse_config_missing_optional_fields() {
        let raw = json!({
            "method": "GET",
            "url_template": "https://api.example.com/"
        });
        let cfg = HttpTool::parse_config(&raw).unwrap();
        assert_eq!(cfg.method, "GET");
        assert!(cfg.headers_template.is_none());
        assert!(cfg.body_template.is_none());
        assert_eq!(cfg.timeout_secs, None);
    }

    #[tokio::test]
    async fn test_invalid_method() {
        let tool = make_test_tool(HttpToolConfig {
            method: "FAKE".to_string(),
            url_template: "https://example.com".to_string(),
            ..Default::default()
        });
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg) if msg.contains("Unsupported HTTP method")
        ));
    }

    #[tokio::test]
    async fn test_empty_url_template() {
        let tool = make_test_tool(HttpToolConfig {
            method: "GET".to_string(),
            url_template: "".to_string(),
            ..Default::default()
        });
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg) if msg.contains("empty")
        ));
    }

    #[tokio::test]
    async fn test_non_object_args_rejected() {
        let tool = make_test_tool(HttpToolConfig::default());
        let err = tool.execute(json!("not-an-object")).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg) if msg.contains("must be a JSON object")
        ));
    }

    #[test]
    fn test_substitute_string_simple() {
        let mut map = serde_json::Map::new();
        map.insert("name".to_string(), json!("world"));
        let result = substitute_template_string("Hello {{name}}!", &map).unwrap();
        assert_eq!(result, "Hello world!");
    }

    #[test]
    fn test_substitute_string_multiple() {
        let mut map = serde_json::Map::new();
        map.insert("a".to_string(), json!("1"));
        map.insert("b".to_string(), json!("2"));
        let result = substitute_template_string("{{a}}-{{b}}", &map).unwrap();
        assert_eq!(result, "1-2");
    }

    #[test]
    fn test_substitute_value_nested() {
        let mut map = serde_json::Map::new();
        map.insert("q".to_string(), json!("rust"));
        let input = json!({ "search": "{{q}}", "nested": { "term": "{{q}}" } });
        let result = substitute_template_value(&input, &map).unwrap();
        assert_eq!(result["search"], "rust");
        assert_eq!(result["nested"]["term"], "rust");
    }

    #[test]
    fn test_substitute_value_array() {
        let mut map = serde_json::Map::new();
        map.insert("x".to_string(), json!("val"));
        let input = json!(["{{x}}", "static"]);
        let result = substitute_template_value(&input, &map).unwrap();
        assert_eq!(result.as_array().unwrap()[0], "val");
        assert_eq!(result.as_array().unwrap()[1], "static");
    }

    #[test]
    fn test_substitute_with_number() {
        let mut map = serde_json::Map::new();
        map.insert("id".to_string(), json!(42));
        let result = substitute_template_string("/items/{{id}}", &map).unwrap();
        assert_eq!(result, "/items/42");
    }

    #[test]
    fn test_substitute_unsupported_array() {
        let mut map = serde_json::Map::new();
        map.insert("bad".to_string(), json!([1, 2, 3]));
        let result = substitute_template_string("{{bad}}", &map);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_request() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/get"))
            .and(query_param("foo", "baz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;

        let tool = make_test_tool(HttpToolConfig {
            method: "GET".to_string(),
            url_template: format!("{}/get?foo={{{{bar}}}}", server.uri()),
            ..Default::default()
        });
        let result = tool.execute(json!({ "bar": "baz" })).await.unwrap();
        assert_eq!(result["status"], 200);
        assert_eq!(result["body"]["ok"], true);
    }

    #[tokio::test]
    async fn test_post_request_with_headers_and_body() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/post"))
            .and(header("X-Custom-Token", "secret123"))
            .and(body_json(json!({ "message": "hello" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "received": true })))
            .mount(&server)
            .await;

        let tool = make_test_tool(HttpToolConfig {
            method: "POST".to_string(),
            url_template: format!("{}/post", server.uri()),
            headers_template: Some(json!({ "X-Custom-Token": "{{token}}" })),
            body_template: Some(json!({ "message": "{{msg}}" })),
            ..Default::default()
        });
        let result = tool
            .execute(json!({ "token": "secret123", "msg": "hello" }))
            .await
            .unwrap();
        assert_eq!(result["status"], 200);
        assert_eq!(result["body"]["received"], true);
    }

    #[tokio::test]
    async fn test_non_200_response_preserved() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/teapot"))
            .respond_with(ResponseTemplate::new(418).set_body_string("I'm a teapot"))
            .mount(&server)
            .await;

        let tool = make_test_tool(HttpToolConfig {
            method: "GET".to_string(),
            url_template: format!("{}/teapot", server.uri()),
            ..Default::default()
        });
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["status"], 418);
        assert_eq!(result["body"], "I'm a teapot");
    }
}
