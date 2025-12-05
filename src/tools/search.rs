//! Search tool implementation using daedra
//!
//! This module provides web search capabilities via the daedra crate,
//! which uses DuckDuckGo as the search backend.

use crate::tools::registry::Tool;
use crate::types::{AppError, Result};
use async_trait::async_trait;
use serde_json::{Value, json};

/// Web search tool powered by daedra
pub struct SearchTool;

impl SearchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for information using DuckDuckGo"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 10)",
                    "default": 10
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::InvalidInput("Missing 'query' parameter".to_string()))?;

        let num_results = args
            .get("num_results")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(10);

        // Use daedra for web search
        let search_args = daedra::SearchArgs {
            query: query.to_string(),
            options: Some(daedra::SearchOptions {
                num_results,
                ..Default::default()
            }),
        };

        match daedra::tools::search::perform_search(&search_args).await {
            Ok(response) => {
                let results: Vec<Value> = response
                    .data
                    .iter()
                    .map(|r| {
                        json!({
                            "title": r.title,
                            "url": r.url,
                            "description": r.description
                        })
                    })
                    .collect();

                Ok(json!({
                    "query": query,
                    "results": results,
                    "count": results.len()
                }))
            }
            Err(e) => Err(AppError::Internal(format!("Search failed: {}", e))),
        }
    }
}

/// Page fetching tool powered by daedra
pub struct FetchPageTool;

impl FetchPageTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for FetchPageTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for FetchPageTool {
    fn name(&self) -> &str {
        "fetch_page"
    }

    fn description(&self) -> &str {
        "Fetch a web page and convert it to markdown"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL of the page to fetch"
                },
                "selector": {
                    "type": "string",
                    "description": "Optional CSS selector to extract specific content"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::InvalidInput("Missing 'url' parameter".to_string()))?;

        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .map(String::from);

        let fetch_args = daedra::VisitPageArgs {
            url: url.to_string(),
            include_images: false,
            selector,
        };

        match daedra::tools::fetch::fetch_page(&fetch_args).await {
            Ok(page_content) => Ok(json!({
                "url": page_content.url,
                "title": page_content.title,
                "content": page_content.content,
                "word_count": page_content.word_count
            })),
            Err(e) => Err(AppError::Internal(format!("Failed to fetch page: {}", e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_tool_definition() {
        let tool = SearchTool::new();
        assert_eq!(tool.name(), "web_search");
        assert!(!tool.description().is_empty());

        let schema = tool.parameters_schema();
        assert!(schema.is_object());
        assert!(schema.get("properties").is_some());
    }

    #[test]
    fn test_fetch_page_tool_definition() {
        let tool = FetchPageTool::new();
        assert_eq!(tool.name(), "fetch_page");
        assert!(!tool.description().is_empty());

        let schema = tool.parameters_schema();
        assert!(schema.is_object());
        assert!(schema.get("properties").is_some());
    }

    #[tokio::test]
    async fn test_search_missing_query() {
        let tool = SearchTool::new();
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fetch_page_missing_url() {
        let tool = FetchPageTool::new();
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }
}
