//! MCP→ToolRegistry bridge.
//!
//! Registers MCP client operations as Tool implementations so agents can
//! call them via their normal tool-calling loop. When an MCP client is
//! configured (e.g., eruka.toon), its operations become agent-callable tools.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::mcp::client::McpClient;
use crate::tools::registry::Tool;
use crate::types::Result;

/// Register all tools from an MCP client into the tool registry.
///
/// Each MCP client method (get_context, write_context, etc.) becomes
/// a separate Tool with the naming convention `{client_name}_{operation}`.
pub fn register_mcp_tools(
    registry: &mut crate::tools::registry::ToolRegistry,
    client_name: &str,
    client: Arc<McpClient>,
) {
    let prefix = client_name.to_string();

    registry.register(Arc::new(McpGetContext {
        name: format!("{}_get_context", prefix),
        client: client.clone(),
    }));
    registry.register(Arc::new(McpWriteContext {
        name: format!("{}_write_context", prefix),
        client: client.clone(),
    }));
    registry.register(Arc::new(McpSearchContext {
        name: format!("{}_search_context", prefix),
        client: client.clone(),
    }));
    registry.register(Arc::new(McpGetCompleteness {
        name: format!("{}_get_completeness", prefix),
        client: client.clone(),
    }));
    registry.register(Arc::new(McpGetGaps {
        name: format!("{}_get_gaps", prefix),
        client: client.clone(),
    }));
    registry.register(Arc::new(McpDetectGaps {
        name: format!("{}_detect_gaps", prefix),
        client: client.clone(),
    }));

    tracing::info!(
        client = %prefix,
        "Registered 6 MCP bridge tools: {prefix}_get_context, {prefix}_write_context, {prefix}_search_context, {prefix}_get_completeness, {prefix}_get_gaps, {prefix}_detect_gaps"
    );
}

// =============================================================================
// Individual MCP bridge tools
// =============================================================================

struct McpGetContext {
    name: String,
    client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpGetContext {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str {
        "Read a context field from the knowledge base. Provide a path like 'identity/company_name' or 'market/competitors'."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Field path (e.g., 'identity/company_name')" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: Value) -> Result<Value> {
        let path = args["path"].as_str().unwrap_or("");
        self.client.get_context(path).await
            .map_err(|e| crate::types::AppError::External(e.to_string()))
    }
}

struct McpWriteContext {
    name: String,
    client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpWriteContext {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str {
        "Write a value to a context field in the knowledge base. Provide path and value."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Field path (e.g., 'identity/company_name')" },
                "value": { "type": "string", "description": "Value to write" }
            },
            "required": ["path", "value"]
        })
    }
    async fn execute(&self, args: Value) -> Result<Value> {
        let path = args["path"].as_str().unwrap_or("");
        let value = args["value"].as_str().unwrap_or("");
        self.client.write_context(path, value).await
            .map_err(|e| crate::types::AppError::External(e.to_string()))
    }
}

struct McpSearchContext {
    name: String,
    client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpSearchContext {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str {
        "Search the knowledge base with a natural language query. Returns matching context fields."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "scope": { "type": "string", "description": "Scope to search within (optional)" },
                "max_results": { "type": "integer", "description": "Maximum results (default 10)" }
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, args: Value) -> Result<Value> {
        let query = args["query"].as_str().unwrap_or("");
        let scope = args["scope"].as_str();
        let max = args["max_results"].as_u64().map(|n| n as usize);
        self.client.search_context(query, scope, max).await
            .map_err(|e| crate::types::AppError::External(e.to_string()))
    }
}

struct McpGetCompleteness {
    name: String,
    client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpGetCompleteness {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str {
        "Get knowledge completeness percentage for the workspace. Shows what percentage of required fields are filled."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "scope": { "type": "string", "description": "Category scope (e.g., 'identity', 'market'). Omit for all." }
            }
        })
    }
    async fn execute(&self, args: Value) -> Result<Value> {
        let scope = args["scope"].as_str();
        self.client.get_completeness(scope).await
            .map_err(|e| crate::types::AppError::External(e.to_string()))
    }
}

struct McpGetGaps {
    name: String,
    client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpGetGaps {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str {
        "List knowledge gaps — fields that are UNKNOWN or UNCERTAIN. Shows what information is missing."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "description": "Filter by state: 'UNKNOWN', 'UNCERTAIN'" },
                "category": { "type": "string", "description": "Filter by category" }
            }
        })
    }
    async fn execute(&self, args: Value) -> Result<Value> {
        let status = args["status"].as_str();
        let category = args["category"].as_str();
        self.client.get_gaps(status, category).await
            .map_err(|e| crate::types::AppError::External(e.to_string()))
    }
}

struct McpDetectGaps {
    name: String,
    client: Arc<McpClient>,
}

#[async_trait]
impl Tool for McpDetectGaps {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str {
        "Detect new knowledge gaps by analyzing what fields should exist but are missing."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "category": { "type": "string", "description": "Category to analyze for gaps" }
            }
        })
    }
    async fn execute(&self, args: Value) -> Result<Value> {
        let category = args["category"].as_str();
        self.client.detect_gaps(category).await
            .map_err(|e| crate::types::AppError::External(e.to_string()))
    }
}
