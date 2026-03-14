use crate::tools::registry::Tool;
use crate::types::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::env;

fn pom_base_url() -> String {
    env::var("POM_BASE_URL").unwrap_or_else(|_| "http://localhost:3002".to_string())
}

// ─── pom_create_dissue ────────────────────────────────────────────────────────

pub struct PomCreateDissueTool {
    client: reqwest::Client,
}

impl PomCreateDissueTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
}

impl Default for PomCreateDissueTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for PomCreateDissueTool {
    fn name(&self) -> &str {
        "pom_create_dissue"
    }

    fn description(&self) -> &str {
        "Create a new Dissue (task/issue) in the POM project management system. Use this to track work items, bugs, features, or action items."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short title for the dissue"
                },
                "description": {
                    "type": "string",
                    "description": "Detailed description (optional)"
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "critical"],
                    "description": "Priority level (default: medium)"
                },
                "sprint_number": {
                    "type": "integer",
                    "description": "Sprint number to assign this dissue to (optional)"
                },
                "assignee": {
                    "type": "string",
                    "description": "Who should work on this (optional)"
                }
            },
            "required": ["title"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let url = format!("{}/api/dissues", pom_base_url());
        let body = json!({
            "title": args.get("title").and_then(|v| v.as_str()).unwrap_or(""),
            "description": args.get("description").and_then(|v| v.as_str()),
            "priority": args.get("priority").and_then(|v| v.as_str()).unwrap_or("medium"),
            "sprint_number": args.get("sprint_number").and_then(|v| v.as_i64()).map(|n| n as i32),
            "assignee": args.get("assignee").and_then(|v| v.as_str()),
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        let status = resp.status();
        let json: Value = resp
            .json()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        if status.is_success() {
            Ok(json!({
                "success": true,
                "dissue": json
            }))
        } else {
            Ok(json!({
                "success": false,
                "error": json
            }))
        }
    }
}

// ─── pom_update_dissue ────────────────────────────────────────────────────────

pub struct PomUpdateDissueTool {
    client: reqwest::Client,
}

impl PomUpdateDissueTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
}

impl Default for PomUpdateDissueTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for PomUpdateDissueTool {
    fn name(&self) -> &str {
        "pom_update_dissue"
    }

    fn description(&self) -> &str {
        "Update an existing Dissue in the POM system. Use this to change status, priority, assignee, or add description."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The dissue ID to update"
                },
                "title": {
                    "type": "string",
                    "description": "New title (optional)"
                },
                "description": {
                    "type": "string",
                    "description": "New description (optional)"
                },
                "status": {
                    "type": "string",
                    "enum": ["open", "in_progress", "done", "blocked", "cancelled"],
                    "description": "New status"
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "critical"],
                    "description": "New priority"
                },
                "assignee": {
                    "type": "string",
                    "description": "New assignee"
                }
            },
            "required": ["id"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let id = args
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::types::AppError::InvalidInput("id is required".into()))?;
        let url = format!("{}/api/dissues/{}", pom_base_url(), id);
        let body = json!({
            "title": args.get("title").and_then(|v| v.as_str()),
            "description": args.get("description").and_then(|v| v.as_str()),
            "status": args.get("status").and_then(|v| v.as_str()),
            "priority": args.get("priority").and_then(|v| v.as_str()),
            "assignee": args.get("assignee").and_then(|v| v.as_str()),
        });
        let resp = self
            .client
            .put(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        let status = resp.status();
        let json: Value = resp
            .json()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        if status.is_success() {
            Ok(json!({ "success": true, "dissue": json }))
        } else {
            Ok(json!({ "success": false, "error": json }))
        }
    }
}

// ─── pom_get_current_sprint ───────────────────────────────────────────────────

pub struct PomGetCurrentSprintTool {
    client: reqwest::Client,
}

impl PomGetCurrentSprintTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
}

impl Default for PomGetCurrentSprintTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for PomGetCurrentSprintTool {
    fn name(&self) -> &str {
        "pom_get_current_sprint"
    }

    fn description(&self) -> &str {
        "Get the current active sprint from POM, including its dissues. Returns markdown-formatted sprint context."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: Value) -> Result<Value> {
        let url = format!("{}/api/current-sprint", pom_base_url());
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        let text = resp
            .text()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        Ok(json!({ "sprint_context": text }))
    }
}

// ─── pom_list_dissues ─────────────────────────────────────────────────────────

pub struct PomListDissuesTool {
    client: reqwest::Client,
}

impl PomListDissuesTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
}

impl Default for PomListDissuesTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for PomListDissuesTool {
    fn name(&self) -> &str {
        "pom_list_dissues"
    }

    fn description(&self) -> &str {
        "List dissues from POM with optional filters. Returns a list of matching dissues."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["open", "in_progress", "done", "blocked", "cancelled"],
                    "description": "Filter by status (optional)"
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "critical"],
                    "description": "Filter by priority (optional)"
                },
                "sprint_number": {
                    "type": "integer",
                    "description": "Filter by sprint number (optional)"
                },
                "q": {
                    "type": "string",
                    "description": "Search term to filter by title/description (optional)"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let mut params = vec!["per_page=50".to_string()];
        if let Some(s) = args.get("status").and_then(|v| v.as_str()) {
            params.push(format!("status={}", s));
        }
        if let Some(p) = args.get("priority").and_then(|v| v.as_str()) {
            params.push(format!("priority={}", p));
        }
        if let Some(n) = args.get("sprint_number").and_then(|v| v.as_i64()) {
            params.push(format!("sprint_number={}", n));
        }
        if let Some(q) = args.get("q").and_then(|v| v.as_str()) {
            // Simple URL encode: replace spaces and special chars
            let encoded = q.replace(' ', "+").replace('&', "%26").replace('=', "%3D");
            params.push(format!("q={}", encoded));
        }
        let url = format!("{}/api/dissues?{}", pom_base_url(), params.join("&"));
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        Ok(json)
    }
}
