use crate::tools::registry::Tool;
use crate::types::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct WebSearch {
    _client: reqwest::Client,
}

impl WebSearch {
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
        let query = args["query"]
            .as_str()
            .ok_or_else(|| crate::types::AppError::InvalidInput("query is required".to_string()))?;

        let max_results = args["max_results"].as_i64().unwrap_or(5) as usize;

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
            .map_err(|e| crate::types::AppError::External(format!("Search failed: {}", e)))?;

        // Convert results to JSON
        let json_results: Vec<Value> = results
            .data
            .into_iter()
            .map(|result| {
                json!({
                    "title": result.title,
                    "url": result.url,
                    "snippet": result.description
                })
            })
            .collect();

        Ok(json!({
            "query": query,
            "results": json_results,
            "count": json_results.len()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn test_missing_query() {
        let tool = WebSearch::new();
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }
}
