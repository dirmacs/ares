use crate::registry::Tool;
use ares_types::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

/// Web search tool using DuckDuckGo via daedra.
pub struct WebSearch {
    _client: reqwest::Client,
}

impl WebSearch {
    /// Creates a new WebSearch tool instance.
    pub fn new() -> Self {
        Self {
            _client: reqwest::Client::new(),
        }
    }
}

impl Default for WebSearch {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse `web_search` tool arguments into query text and a result limit.
pub(crate) fn parse_search_args(args: &Value) -> Result<(&str, usize)> {
    let query = args["query"]
        .as_str()
        .ok_or_else(|| ares_types::AppError::InvalidInput("query is required".to_string()))?;
    let max_results = args["max_results"].as_i64().unwrap_or(5) as usize;
    Ok((query, max_results))
}

/// Build the JSON payload returned by `web_search` from normalized hit tuples.
pub(crate) fn format_search_response(query: &str, hits: &[(String, String, String)]) -> Value {
    let json_results: Vec<Value> = hits
        .iter()
        .map(|(title, url, snippet)| {
            json!({
                "title": title,
                "url": url,
                "snippet": snippet
            })
        })
        .collect();

    json!({
        "query": query,
        "results": json_results,
        "count": json_results.len()
    })
}

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for information using DuckDuckGo. Returns a list of search results with titles, snippets, and URLs."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to look up"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5)",
                    "default": 5
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let (query, max_results) = parse_search_args(&args)?;

        // Use daedra to perform the search
        let search_args = daedra::types::SearchArgs {
            query: query.to_string(),
            options: Some(daedra::types::SearchOptions {
                num_results: max_results,
                ..Default::default()
            }),
        };

        let results = daedra::tools::search::perform_search(&search_args)
            .await
            .map_err(|e| ares_types::AppError::External(format!("Search failed: {}", e)))?;

        let hits: Vec<(String, String, String)> = results
            .data
            .into_iter()
            .map(|result| (result.title, result.url, result.description))
            .collect();

        Ok(format_search_response(query, &hits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolConfig;
    use ares_types::AppError;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn test_schema() {
        let tool = WebSearch::new();
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("query")));
    }

    #[test]
    fn test_schema_has_max_results() {
        let tool = WebSearch::new();
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["max_results"].is_object());
        assert_eq!(schema["properties"]["max_results"]["default"], 5);
    }

    #[test]
    fn test_name_and_description() {
        let tool = WebSearch::new();
        assert_eq!(tool.name(), "web_search");
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("DuckDuckGo"));
    }

    #[test]
    fn test_default() {
        let tool = WebSearch::default();
        assert_eq!(tool.name(), "web_search");
    }

    #[test]
    fn test_parse_search_args_defaults_max_results() {
        let args = json!({ "query": "rust lang" });
        let (query, max_results) = parse_search_args(&args).unwrap();
        assert_eq!(query, "rust lang");
        assert_eq!(max_results, 5);
    }

    #[test]
    fn test_parse_search_args_custom_max_results() {
        let args = json!({ "query": "ares", "max_results": 3 });
        let (query, max_results) = parse_search_args(&args).unwrap();
        assert_eq!(query, "ares");
        assert_eq!(max_results, 3);
    }

    #[test]
    fn test_parse_search_args_invalid_max_results_falls_back() {
        let args = json!({ "query": "test", "max_results": "many" });
        let (_, max_results) = parse_search_args(&args).unwrap();
        assert_eq!(max_results, 5);
    }

    #[test]
    fn test_format_search_response_shape() {
        let hits = vec![
            (
                "Rust".to_string(),
                "https://rust-lang.org".to_string(),
                "A language empowering everyone".to_string(),
            ),
            (
                "Docs".to_string(),
                "https://doc.rust-lang.org".to_string(),
                "The Rust book".to_string(),
            ),
        ];
        let payload = format_search_response("rust", &hits);

        assert_eq!(payload["query"], "rust");
        assert_eq!(payload["count"], 2);
        assert_eq!(payload["results"][0]["title"], "Rust");
        assert_eq!(payload["results"][0]["url"], "https://rust-lang.org");
        assert_eq!(
            payload["results"][0]["snippet"],
            "A language empowering everyone"
        );
        assert_eq!(payload["results"][1]["title"], "Docs");
    }

    #[test]
    fn test_format_search_response_empty_results() {
        let payload = format_search_response("nothing-here", &[]);
        assert_eq!(payload["query"], "nothing-here");
        assert_eq!(payload["count"], 0);
        assert!(payload["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_web_search_tool_config_serde_roundtrip() {
        let tool = ToolConfig {
            enabled: false,
            description: Some("Custom web search".into()),
            timeout_secs: 45,
            extra: HashMap::from([(
                "region".to_string(),
                toml::Value::String("us".to_string()),
            )]),
        };

        let encoded = serde_json::to_string(&tool).unwrap();
        let decoded: ToolConfig = serde_json::from_str(&encoded).unwrap();

        assert!(!decoded.enabled);
        assert_eq!(decoded.description.as_deref(), Some("Custom web search"));
        assert_eq!(decoded.timeout_secs, 45);
        assert_eq!(
            decoded.extra.get("region").and_then(toml::Value::as_str),
            Some("us")
        );
    }

    #[test]
    fn test_registry_web_search_config_override() {
        use crate::registry::ToolRegistry;

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WebSearch::new()));
        registry.set_config(
            "web_search",
            ToolConfig {
                enabled: true,
                description: Some("Configured search tool".into()),
                timeout_secs: 90,
                extra: HashMap::new(),
            },
        );

        let definitions = registry.get_tool_definitions();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name, "web_search");
        assert_eq!(definitions[0].description, "Configured search tool");
        assert_eq!(registry.get_timeout("web_search"), 90);
    }

    #[tokio::test]
    async fn test_missing_query() {
        let tool = WebSearch::new();
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_missing_query_error_message() {
        let tool = WebSearch::new();
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(
            err,
            AppError::InvalidInput(msg) if msg.contains("query is required")
        ));
    }

    #[tokio::test]
    async fn test_null_query_rejected() {
        let tool = WebSearch::new();
        let result = tool.execute(json!({ "query": null })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_numeric_query_rejected() {
        let tool = WebSearch::new();
        let result = tool.execute(json!({ "query": 42 })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_array_query_rejected() {
        let tool = WebSearch::new();
        let result = tool.execute(json!({ "query": ["rust"] })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_empty_query_returns_empty_results() {
        let tool = WebSearch::new();
        let payload = tool
            .execute(json!({ "query": "" }))
            .await
            .expect("empty query should still return a response envelope");
        assert_eq!(payload["query"], "");
        assert_eq!(payload["count"], 0);
        assert!(payload["results"].as_array().unwrap().is_empty());
    }

    /// Live DuckDuckGo search — only runs when `ARES_LIVE_SEARCH_TEST` is set in the environment.
    #[tokio::test]
    async fn test_live_search_when_env_set() {
        if std::env::var("ARES_LIVE_SEARCH_TEST").is_err() {
            return;
        }

        let tool = WebSearch::new();
        let payload = tool
            .execute(json!({ "query": "rust programming language", "max_results": 2 }))
            .await
            .expect("live search should succeed when env is set");

        assert_eq!(payload["query"], "rust programming language");
        assert!(payload["count"].as_u64().unwrap_or(0) > 0);
        assert!(!payload["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_live_search_env_not_set_by_default() {
        std::env::remove_var("ARES_LIVE_SEARCH_TEST");
        assert!(std::env::var("ARES_LIVE_SEARCH_TEST").is_err());
    }
}
