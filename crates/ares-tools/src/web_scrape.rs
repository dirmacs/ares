//! Web scraping tool — fetch a URL and extract readable text content.
//!
//! Uses reqwest for HTTP fetching and scraper for HTML parsing.
//! Strips scripts, styles, and navigation to return clean text.

use crate::registry::Tool;
use ares_types::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

/// Web scraping tool that fetches a URL and extracts clean text content.
pub struct WebScrape {
    client: reqwest::Client,
}

impl WebScrape {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("ARES/0.7 (web-scrape-tool)")
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for WebScrape {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebScrape {
    fn name(&self) -> &str {
        "web_scrape"
    }

    fn description(&self) -> &str {
        "Fetch a web page and extract its readable text content. Strips HTML tags, scripts, styles, and navigation elements. Returns clean text suitable for analysis."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch and extract content from"
                },
                "max_length": {
                    "type": "integer",
                    "description": "Maximum characters to return (default: 5000)",
                    "default": 5000
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let url = args["url"]
            .as_str()
            .ok_or_else(|| ares_types::AppError::InvalidInput("url is required".to_string()))?;

        let max_length = args["max_length"].as_i64().unwrap_or(5000) as usize;

        // Fetch the page
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ares_types::AppError::External(format!("Fetch failed: {}", e)))?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Ok(json!({
                "url": url,
                "error": format!("HTTP {}", status),
                "content": ""
            }));
        }

        let html = response
            .text()
            .await
            .map_err(|e| ares_types::AppError::External(format!("Read failed: {}", e)))?;

        // Parse and extract text
        let document = scraper::Html::parse_document(&html);

        // Extract title
        let title_selector = scraper::Selector::parse("title").unwrap();
        let title = document
            .select(&title_selector)
            .next()
            .map(|el| el.text().collect::<String>())
            .unwrap_or_default();

        // Remove script and style elements, collect text from body
        let body_selector = scraper::Selector::parse("body").unwrap();
        let script_selector =
            scraper::Selector::parse("script, style, nav, header, footer").unwrap();

        let mut text = String::new();
        if let Some(body) = document.select(&body_selector).next() {
            // Get all text nodes, skipping script/style/nav elements
            let skip_ids: std::collections::HashSet<_> = document
                .select(&script_selector)
                .map(|el| el.id())
                .collect();

            for node in body.descendants() {
                if let Some(el) = node.value().as_element() {
                    if skip_ids.contains(&node.id()) {
                        continue;
                    }
                    // Add newlines for block elements
                    let tag = el.name();
                    if matches!(
                        tag,
                        "p" | "div" | "br" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li" | "tr"
                    )
                        && !text.ends_with('\n') {
                            text.push('\n');
                        }
                } else if let Some(t) = node.value().as_text() {
                    // Check if any ancestor is a script/style
                    let mut skip = false;
                    let mut parent = node.parent();
                    while let Some(p) = parent {
                        if skip_ids.contains(&p.id()) {
                            skip = true;
                            break;
                        }
                        parent = p.parent();
                    }
                    if !skip {
                        text.push_str(t.trim());
                        if !t.trim().is_empty() {
                            text.push(' ');
                        }
                    }
                }
            }
        }

        // Clean up whitespace
        let text = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");

        // Truncate if needed
        let text = if text.len() > max_length {
            format!("{}...", &text[..max_length])
        } else {
            text
        };

        Ok(json!({
            "url": url,
            "title": title.trim(),
            "content": text,
            "length": text.len()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = WebScrape::new();
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["url"].is_object());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("url")));
    }

    #[test]
    fn test_schema_has_max_length() {
        let tool = WebScrape::new();
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["max_length"].is_object());
        assert_eq!(schema["properties"]["max_length"]["default"], 5000);
    }

    #[tokio::test]
    async fn test_missing_url() {
        let tool = WebScrape::new();
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_missing_url_error_message() {
        let tool = WebScrape::new();
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg) if msg.contains("url is required")
        ));
    }

    #[tokio::test]
    async fn test_empty_url() {
        let tool = WebScrape::new();
        let result = tool.execute(json!({"url": ""})).await;
        // Empty URL should fail at the HTTP level
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_invalid_url() {
        let tool = WebScrape::new();
        let result = tool.execute(json!({"url": "not-a-valid-url"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_nonexistent_host() {
        let tool = WebScrape::new();
        let result = tool
            .execute(json!({"url": "http://this-host-definitely-does-not-exist-xyz123.com"}))
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_name_and_description() {
        let tool = WebScrape::new();
        assert_eq!(tool.name(), "web_scrape");
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("readable text"));
    }

    #[test]
    fn test_default() {
        let tool = WebScrape::default();
        assert_eq!(tool.name(), "web_scrape");
    }

    #[tokio::test]
    async fn test_null_url_rejected() {
        let tool = WebScrape::new();
        let result = tool.execute(json!({"url": null})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_numeric_url_rejected() {
        let tool = WebScrape::new();
        let result = tool.execute(json!({"url": 12345})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_array_url_rejected() {
        let tool = WebScrape::new();
        let result = tool.execute(json!({"url": ["https://example.com"]})).await;
        assert!(result.is_err());
    }
}
