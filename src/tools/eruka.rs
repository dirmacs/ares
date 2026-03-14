// ares/src/tools/eruka.rs
// Eruka context read/write tools for agents.
// Agents use these to read structured business knowledge from Eruka.

use crate::tools::registry::Tool;
use crate::types::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::env;

fn eruka_base_url() -> String {
    env::var("ERUKA_API_URL").unwrap_or_else(|_| "http://localhost:8081".to_string())
}

fn eruka_jwt() -> Option<String> {
    env::var("ERUKA_JWT_TOKEN").ok()
}

async fn eruka_get(path: &str) -> Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| crate::types::AppError::External(format!("HTTP client: {}", e)))?;

    let url = format!("{}/api/v1/context?path={}", eruka_base_url(), path);
    let mut req = client.get(&url);
    if let Some(jwt) = eruka_jwt() {
        req = req.header("Authorization", format!("Bearer {}", jwt));
    }

    let resp = req.send().await
        .map_err(|e| crate::types::AppError::External(format!("Eruka request failed: {}", e)))?;

    if resp.status().is_success() {
        let val: Value = resp.json().await
            .map_err(|e| crate::types::AppError::External(format!("Eruka parse: {}", e)))?;
        Ok(val)
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok(json!({ "error": format!("HTTP {}: {}", status, text) }))
    }
}

async fn eruka_search_ctx(query: &str, max_results: usize) -> Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| crate::types::AppError::External(format!("HTTP client: {}", e)))?;

    let url = format!("{}/api/v1/context/search", eruka_base_url());
    let body = json!({ "query": query, "max_results": max_results });
    let mut req = client.post(&url).json(&body);
    if let Some(jwt) = eruka_jwt() {
        req = req.header("Authorization", format!("Bearer {}", jwt));
    }

    let resp = req.send().await
        .map_err(|e| crate::types::AppError::External(format!("Eruka search failed: {}", e)))?;

    let val: Value = resp.json().await
        .map_err(|e| crate::types::AppError::External(format!("Eruka parse: {}", e)))?;
    Ok(val)
}

// ─── eruka_read ───────────────────────────────────────────────────────────────

pub struct ErukaRead;

#[async_trait]
impl Tool for ErukaRead {
    fn name(&self) -> &str { "eruka_read" }

    fn description(&self) -> &str {
        "Read a specific field or category from the Eruka knowledge base. Use dot-notation paths like 'identity/company_name' or 'market/icp_description'."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Eruka field path (e.g. 'identity/company_name', 'content/dtrain_overview', 'survey/questions/operations')"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let path = args["path"].as_str()
            .ok_or_else(|| crate::types::AppError::InvalidInput("path required".into()))?;
        eruka_get(path).await
    }
}

// ─── eruka_search ─────────────────────────────────────────────────────────────

pub struct ErukaSearch;

#[async_trait]
impl Tool for ErukaSearch {
    fn name(&self) -> &str { "eruka_search" }

    fn description(&self) -> &str {
        "Search the Eruka knowledge base by query. Returns semantically relevant context fields."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query (e.g. 'operations survey questions', 'sales automation')"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results to return (default: 5)",
                    "default": 5
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let query = args["query"].as_str()
            .ok_or_else(|| crate::types::AppError::InvalidInput("query required".into()))?;
        let max = args["max_results"].as_u64().unwrap_or(5) as usize;
        eruka_search_ctx(query, max).await
    }
}
