//! MCP Bridge tools for A.R.E.S.
//!
//! These tools bridge A.R.E.S. to external MCP servers, allowing tools to query
//! context, gaps, and completeness from MCP-compatible servers.

use crate::registry::{Tool, ToolRegistry};
use ares_mcp::client::{McpClient, McpServerConfig};
use ares_types::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Get context from an MCP server.
pub struct McpGetContext {
    client: McpClient,
}

impl McpGetContext {
    pub fn new(client: McpClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for McpGetContext {
    fn name(&self) -> &str {
        "mcp_get_context"
    }

    fn description(&self) -> &str {
        "Get context from an MCP server by path"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to retrieve context from"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| ares_types::AppError::InvalidInput("path is required".to_string()))?;

        self.client
            .get_context(path)
            .await
            .map_err(|e| ares_types::AppError::External(e.to_string()))
    }
}

/// Write context to an MCP server.
pub struct McpWriteContext {
    client: McpClient,
}

impl McpWriteContext {
    pub fn new(client: McpClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for McpWriteContext {
    fn name(&self) -> &str {
        "mcp_write_context"
    }

    fn description(&self) -> &str {
        "Write context to an MCP server at a specific path"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path to write context to"
                },
                "value": {
                    "type": "string",
                    "description": "The value to write"
                }
            },
            "required": ["path", "value"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| ares_types::AppError::InvalidInput("path is required".to_string()))?;
        let value = args["value"]
            .as_str()
            .ok_or_else(|| ares_types::AppError::InvalidInput("value is required".to_string()))?;

        self.client
            .write_context(path, value)
            .await
            .map_err(|e| ares_types::AppError::External(e.to_string()))
    }
}

/// Search context in an MCP server.
pub struct McpSearchContext {
    client: McpClient,
}

impl McpSearchContext {
    pub fn new(client: McpClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for McpSearchContext {
    fn name(&self) -> &str {
        "mcp_search_context"
    }

    fn description(&self) -> &str {
        "Search context in an MCP server with optional scope and max results"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "scope": {
                    "type": "string",
                    "description": "Optional scope to limit search (e.g., 'workspace')"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return",
                    "default": 10
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| ares_types::AppError::InvalidInput("query is required".to_string()))?;

        let scope = args["scope"].as_str();
        let max_results = args["max_results"].as_i64().map(|m| m as usize);

        self.client
            .search_context(query, scope, max_results)
            .await
            .map_err(|e| ares_types::AppError::External(e.to_string()))
    }
}

/// Get completeness from an MCP server.
pub struct McpGetCompleteness {
    client: McpClient,
}

impl McpGetCompleteness {
    pub fn new(client: McpClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for McpGetCompleteness {
    fn name(&self) -> &str {
        "mcp_get_completeness"
    }

    fn description(&self) -> &str {
        "Get completeness metrics from an MCP server for a scope"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "description": "Optional scope to get completeness for (defaults to '*')",
                    "default": "*"
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let scope = args["scope"].as_str();

        self.client
            .get_completeness(scope)
            .await
            .map_err(|e| ares_types::AppError::External(e.to_string()))
    }
}

/// Get gaps from an MCP server.
pub struct McpGetGaps {
    client: McpClient,
}

impl McpGetGaps {
    pub fn new(client: McpClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for McpGetGaps {
    fn name(&self) -> &str {
        "mcp_get_gaps"
    }

    fn description(&self) -> &str {
        "Get gaps from an MCP server filtered by status and category"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "description": "Optional status filter (e.g., 'open', 'closed')"
                },
                "category": {
                    "type": "string",
                    "description": "Optional category filter"
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let status = args["status"].as_str();
        let category = args["category"].as_str();

        self.client
            .get_gaps(status, category)
            .await
            .map_err(|e| ares_types::AppError::External(e.to_string()))
    }
}

/// Detect gaps in an MCP server.
pub struct McpDetectGaps {
    client: McpClient,
}

impl McpDetectGaps {
    pub fn new(client: McpClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Tool for McpDetectGaps {
    fn name(&self) -> &str {
        "mcp_detect_gaps"
    }

    fn description(&self) -> &str {
        "Detect gaps in an MCP server optionally filtered by category"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "description": "Optional category to detect gaps for"
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let category = args["category"].as_str();

        self.client
            .detect_gaps(category)
            .await
            .map_err(|e| ares_types::AppError::External(e.to_string()))
    }
}

/// Register all MCP bridge tools into a ToolRegistry.
pub fn register_mcp_tools(registry: &mut ToolRegistry, client_name: &str) {
    let config = McpServerConfig {
        name: client_name.to_string(),
        enabled: true,
        command: None,
        args: None,
        timeout_secs: None,
        endpoint: Some("http://127.0.0.1:9999".to_string()),
        transport: Some("http".to_string()),
        api_key: None,
    };

    registry.register(Arc::new(McpGetContext::new(McpClient::new(config.clone()))));
    registry.register(Arc::new(McpWriteContext::new(McpClient::new(config.clone()))));
    registry.register(Arc::new(McpSearchContext::new(McpClient::new(config.clone()))));
    registry.register(Arc::new(McpGetCompleteness::new(McpClient::new(config.clone()))));
    registry.register(Arc::new(McpGetGaps::new(McpClient::new(config.clone()))));
    registry.register(Arc::new(McpDetectGaps::new(McpClient::new(config.clone()))));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> McpClient {
        McpClient::new(McpServerConfig {
            name: "test_client".into(),
            enabled: true,
            command: None,
            args: None,
            timeout_secs: None,
            endpoint: Some("http://127.0.0.1:9999".into()),
            transport: Some("http".into()),
            api_key: None,
        })
    }

    // ===============================
    // register_mcp_tools tests (1 test)
    // ===============================

    #[test]
    fn test_register_mcp_tools_registers_all_tools() {
        let mut registry = ToolRegistry::new();
        register_mcp_tools(&mut registry, "test_client");

        assert!(registry.has_tool("mcp_get_context"));
        assert!(registry.has_tool("mcp_write_context"));
        assert!(registry.has_tool("mcp_search_context"));
        assert!(registry.has_tool("mcp_get_completeness"));
        assert!(registry.has_tool("mcp_get_gaps"));
        assert!(registry.has_tool("mcp_detect_gaps"));
    }

    // ===============================
    // McpGetContext tests (3 tests)
    // ===============================

    #[test]
    fn test_mcp_get_context_name() {
        let tool = McpGetContext::new(test_client());
        assert_eq!(tool.name(), "mcp_get_context");
    }

    #[test]
    fn test_mcp_get_context_description() {
        let tool = McpGetContext::new(test_client());
        assert_eq!(tool.description(), "Get context from an MCP server by path");
    }

    #[test]
    fn test_mcp_get_context_parameters_schema() {
        let tool = McpGetContext::new(test_client());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["required"].as_array().unwrap().contains(&json!("path")));
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert_eq!(schema["properties"]["path"]["description"], "The path to retrieve context from");
    }

    #[tokio::test]
    async fn test_mcp_get_context_execute_missing_path() {
        let tool = McpGetContext::new(test_client());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        if let Err(ares_types::AppError::InvalidInput(msg)) = result {
            assert_eq!(msg, "path is required");
        } else {
            panic!("Expected InvalidInput error");
        }
    }

    // ===============================
    // McpWriteContext tests (3 tests)
    // ===============================

    #[test]
    fn test_mcp_write_context_name() {
        let tool = McpWriteContext::new(test_client());
        assert_eq!(tool.name(), "mcp_write_context");
    }

    #[test]
    fn test_mcp_write_context_description() {
        let tool = McpWriteContext::new(test_client());
        assert_eq!(tool.description(), "Write context to an MCP server at a specific path");
    }

    #[test]
    fn test_mcp_write_context_parameters_schema() {
        let tool = McpWriteContext::new(test_client());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["value"].is_object());
        assert!(schema["required"].as_array().unwrap().contains(&json!("path")));
        assert!(schema["required"].as_array().unwrap().contains(&json!("value")));
    }

    #[tokio::test]
    async fn test_mcp_write_context_execute_missing_path() {
        let tool = McpWriteContext::new(test_client());
        let result = tool.execute(json!({"value": "test"})).await;
        assert!(result.is_err());
        if let Err(ares_types::AppError::InvalidInput(msg)) = result {
            assert_eq!(msg, "path is required");
        } else {
            panic!("Expected InvalidInput error");
        }
    }

    // ===============================
    // McpSearchContext tests (3 tests)
    // ===============================

    #[test]
    fn test_mcp_search_context_name() {
        let tool = McpSearchContext::new(test_client());
        assert_eq!(tool.name(), "mcp_search_context");
    }

    #[test]
    fn test_mcp_search_context_description() {
        let tool = McpSearchContext::new(test_client());
        assert_eq!(tool.description(), "Search context in an MCP server with optional scope and max results");
    }

    #[test]
    fn test_mcp_search_context_parameters_schema() {
        let tool = McpSearchContext::new(test_client());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["properties"]["scope"].is_object());
        assert!(schema["properties"]["max_results"].is_object());
        assert!(schema["required"].as_array().unwrap().contains(&json!("query")));
        assert_eq!(schema["properties"]["query"]["type"], "string");
        assert_eq!(schema["properties"]["max_results"]["type"], "integer");
    }

    #[tokio::test]
    async fn test_mcp_search_context_execute_missing_query() {
        let tool = McpSearchContext::new(test_client());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        if let Err(ares_types::AppError::InvalidInput(msg)) = result {
            assert_eq!(msg, "query is required");
        } else {
            panic!("Expected InvalidInput error");
        }
    }

    // ===============================
    // McpGetCompleteness tests (3 tests)
    // ===============================

    #[test]
    fn test_mcp_get_completeness_name() {
        let tool = McpGetCompleteness::new(test_client());
        assert_eq!(tool.name(), "mcp_get_completeness");
    }

    #[test]
    fn test_mcp_get_completeness_description() {
        let tool = McpGetCompleteness::new(test_client());
        assert_eq!(tool.description(), "Get completeness metrics from an MCP server for a scope");
    }

    #[test]
    fn test_mcp_get_completeness_parameters_schema() {
        let tool = McpGetCompleteness::new(test_client());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["scope"].is_object());
        assert_eq!(schema["properties"]["scope"]["type"], "string");
        assert_eq!(schema["properties"]["scope"]["default"], "*");
    }

    #[tokio::test]
    async fn test_mcp_get_completeness_execute_returns_error() {
        let tool = McpGetCompleteness::new(test_client());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        matches!(result.unwrap_err(), ares_types::AppError::External(_));
    }

    // ===============================
    // McpGetGaps tests (3 tests)
    // ===============================

    #[test]
    fn test_mcp_get_gaps_name() {
        let tool = McpGetGaps::new(test_client());
        assert_eq!(tool.name(), "mcp_get_gaps");
    }

    #[test]
    fn test_mcp_get_gaps_description() {
        let tool = McpGetGaps::new(test_client());
        assert_eq!(tool.description(), "Get gaps from an MCP server filtered by status and category");
    }

    #[test]
    fn test_mcp_get_gaps_parameters_schema() {
        let tool = McpGetGaps::new(test_client());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["status"].is_object());
        assert!(schema["properties"]["category"].is_object());
        assert_eq!(schema["properties"]["status"]["type"], "string");
        assert_eq!(schema["properties"]["category"]["type"], "string");
    }

    #[tokio::test]
    async fn test_mcp_get_gaps_execute_returns_error() {
        let tool = McpGetGaps::new(test_client());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        matches!(result.unwrap_err(), ares_types::AppError::External(_));
    }

    // ===============================
    // McpDetectGaps tests (3 tests)
    // ===============================

    #[test]
    fn test_mcp_detect_gaps_name() {
        let tool = McpDetectGaps::new(test_client());
        assert_eq!(tool.name(), "mcp_detect_gaps");
    }

    #[test]
    fn test_mcp_detect_gaps_description() {
        let tool = McpDetectGaps::new(test_client());
        assert_eq!(tool.description(), "Detect gaps in an MCP server optionally filtered by category");
    }

    #[test]
    fn test_mcp_detect_gaps_parameters_schema() {
        let tool = McpDetectGaps::new(test_client());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["category"].is_object());
        assert_eq!(schema["properties"]["category"]["type"], "string");
    }

    #[tokio::test]
    async fn test_mcp_detect_gaps_execute_returns_error() {
        let tool = McpDetectGaps::new(test_client());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        matches!(result.unwrap_err(), ares_types::AppError::External(_));
    }
}