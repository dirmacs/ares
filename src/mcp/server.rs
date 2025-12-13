//! MCP (Model Context Protocol) Server Implementation
//!
//! This module provides an MCP server implementation using the `rmcp` crate,
//! bridging the existing ARES tools to MCP-compatible tools.
//!
//! # Features
//!
//! Enable with the `mcp` feature flag:
//!
//! ```toml
//! ares = { version = "0.1", features = ["mcp"] }
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::mcp::McpServer;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     McpServer::start().await?;
//!     Ok(())
//! }
//! ```

use rmcp::{
    ServerHandler, ServiceExt,
    model::*,
    service::{RequestContext, RoleServer},
    transport::stdio,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use std::sync::Arc;
use tokio::sync::Mutex;

/// Arguments for the calculator tool
#[derive(Debug, Deserialize, Serialize)]
pub struct CalculatorArgs {
    /// The arithmetic operation to perform
    pub operation: String,
    /// First operand
    pub a: f64,
    /// Second operand
    pub b: f64,
}

/// Arguments for the web search tool
#[derive(Debug, Deserialize, Serialize)]
pub struct WebSearchArgs {
    /// The search query
    pub query: String,
    /// Maximum number of results (default: 5)
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

fn default_max_results() -> usize {
    5
}

/// MCP Server implementation that bridges ARES tools to MCP
#[derive(Clone)]
pub struct McpServer {
    /// Internal state for tracking operations
    operation_count: Arc<Mutex<u64>>,
}

impl McpServer {
    /// Create a new MCP server instance
    pub fn new() -> Self {
        Self {
            operation_count: Arc::new(Mutex::new(0)),
        }
    }

    /// Get list of available tools
    fn get_tools() -> Vec<Tool> {
        vec![
            Tool {
                name: "calculator".into(),
                description: Some(
                    "Perform basic arithmetic operations (add, subtract, multiply, divide)".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "operation": {
                            "type": "string",
                            "enum": ["add", "subtract", "multiply", "divide"],
                            "description": "The arithmetic operation to perform"
                        },
                        "a": {
                            "type": "number",
                            "description": "First operand"
                        },
                        "b": {
                            "type": "number",
                            "description": "Second operand"
                        }
                    },
                    "required": ["operation", "a", "b"]
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Calculator".into()),
            },
            Tool {
                name: "web_search".into(),
                description: Some(
                    "Search the web for information using DuckDuckGo. Returns a list of search results with titles, snippets, and URLs.".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query"
                        },
                        "max_results": {
                            "type": "integer",
                            "description": "Maximum number of results (default: 5)",
                            "default": 5
                        }
                    },
                    "required": ["query"]
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Web Search".into()),
            },
            Tool {
                name: "server_stats".into(),
                description: Some(
                    "Get statistics about the MCP server including operation count".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {}
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Server Stats".into()),
            },
            Tool {
                name: "echo".into(),
                description: Some("Echo back the input message (useful for testing)".into()),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "The message to echo back"
                        }
                    },
                    "required": ["message"]
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Echo".into()),
            },
        ]
    }

    /// Execute the calculator tool
    async fn execute_calculator(&self, args: CalculatorArgs) -> CallToolResult {
        let mut count = self.operation_count.lock().await;
        *count += 1;

        let result = match args.operation.as_str() {
            "add" => args.a + args.b,
            "subtract" => args.a - args.b,
            "multiply" => args.a * args.b,
            "divide" => {
                if args.b == 0.0 {
                    return CallToolResult::error(vec![Content::text("Error: Division by zero")]);
                }
                args.a / args.b
            }
            op => {
                return CallToolResult::error(vec![Content::text(format!(
                    "Error: Unknown operation '{}'. Supported: add, subtract, multiply, divide",
                    op
                ))]);
            }
        };

        let response = json!({
            "operation": args.operation,
            "a": args.a,
            "b": args.b,
            "result": result
        });

        CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&response).unwrap_or_else(|_| result.to_string()),
        )])
    }

    /// Execute the web search tool
    async fn execute_web_search(&self, args: WebSearchArgs) -> CallToolResult {
        let mut count = self.operation_count.lock().await;
        *count += 1;

        // Use daedra to perform the search
        let search_args = daedra::types::SearchArgs {
            query: args.query.clone(),
            options: Some(daedra::types::SearchOptions {
                num_results: args.max_results,
                ..Default::default()
            }),
        };

        match daedra::tools::search::perform_search(&search_args).await {
            Ok(results) => {
                let json_results: Vec<serde_json::Value> = results
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

                let response = json!({
                    "query": args.query,
                    "results": json_results,
                    "count": json_results.len()
                });

                CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&response)
                        .unwrap_or_else(|_| "Search completed".to_string()),
                )])
            }
            Err(e) => CallToolResult::error(vec![Content::text(format!("Search failed: {}", e))]),
        }
    }

    /// Execute the server stats tool
    async fn execute_server_stats(&self) -> CallToolResult {
        let count = self.operation_count.lock().await;

        let response = json!({
            "server": "ARES MCP Server",
            "version": env!("CARGO_PKG_VERSION"),
            "operation_count": *count,
            "available_tools": ["calculator", "web_search", "server_stats", "echo"]
        });

        CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&response).unwrap_or_else(|_| "Stats unavailable".into()),
        )])
    }

    /// Execute the echo tool
    async fn execute_echo(&self, message: String) -> CallToolResult {
        let mut count = self.operation_count.lock().await;
        *count += 1;

        CallToolResult::success(vec![Content::text(message)])
    }

    /// Execute a tool by name
    async fn execute_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> CallToolResult {
        let args = arguments.unwrap_or_default();
        let args_value = serde_json::Value::Object(args);

        match name {
            "calculator" => match serde_json::from_value::<CalculatorArgs>(args_value) {
                Ok(calc_args) => self.execute_calculator(calc_args).await,
                Err(e) => CallToolResult::error(vec![Content::text(format!(
                    "Invalid calculator arguments: {}",
                    e
                ))]),
            },
            "web_search" => match serde_json::from_value::<WebSearchArgs>(args_value) {
                Ok(search_args) => self.execute_web_search(search_args).await,
                Err(e) => CallToolResult::error(vec![Content::text(format!(
                    "Invalid search arguments: {}",
                    e
                ))]),
            },
            "server_stats" => self.execute_server_stats().await,
            "echo" => {
                let message = args_value
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.execute_echo(message).await
            }
            _ => CallToolResult::error(vec![Content::text(format!("Unknown tool: {}", name))]),
        }
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Implement ServerHandler for MCP protocol
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "A.R.E.S MCP Server - Provides calculator, web search, and utility tools".into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult {
            tools: Self::get_tools(),
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(self.execute_tool(&request.name, request.arguments).await)
    }
}

impl McpServer {
    /// Start the MCP server with stdio transport
    ///
    /// This function blocks until the server is shut down.
    ///
    /// # Errors
    ///
    /// Returns an error if the server fails to start or encounters a fatal error.
    pub async fn start() -> crate::types::Result<()> {
        tracing::info!("Starting A.R.E.S MCP Server v{}", env!("CARGO_PKG_VERSION"));

        let server = McpServer::new();

        // Serve using stdio transport (standard for MCP)
        let service = server
            .serve(stdio())
            .await
            .map_err(|e| crate::types::AppError::External(format!("MCP server error: {}", e)))?;

        tracing::info!("MCP server started successfully");

        // Wait for the service to complete
        service
            .waiting()
            .await
            .map_err(|e| crate::types::AppError::External(format!("MCP server error: {}", e)))?;

        tracing::info!("MCP server shut down");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculator_args_parsing() {
        let json = r#"{"operation": "add", "a": 5.0, "b": 3.0}"#;
        let args: CalculatorArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.operation, "add");
        assert_eq!(args.a, 5.0);
        assert_eq!(args.b, 3.0);
    }

    #[test]
    fn test_web_search_args_default() {
        let json = r#"{"query": "test query"}"#;
        let args: WebSearchArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.query, "test query");
        assert_eq!(args.max_results, 5); // default value
    }

    #[test]
    fn test_web_search_args_with_max_results() {
        let json = r#"{"query": "test query", "max_results": 10}"#;
        let args: WebSearchArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.query, "test query");
        assert_eq!(args.max_results, 10);
    }

    #[test]
    fn test_mcp_server_creation() {
        let server = McpServer::new();
        // Just verify it can be created
        let _ = server;
    }

    #[test]
    fn test_mcp_server_default() {
        let server = McpServer::default();
        let _ = server;
    }

    #[test]
    fn test_get_tools() {
        let tools = McpServer::get_tools();
        assert_eq!(tools.len(), 4);

        let tool_names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        assert!(tool_names.contains(&"calculator".to_string()));
        assert!(tool_names.contains(&"web_search".to_string()));
        assert!(tool_names.contains(&"server_stats".to_string()));
        assert!(tool_names.contains(&"echo".to_string()));
    }

    #[tokio::test]
    async fn test_calculator_add() {
        let server = McpServer::new();
        let args = CalculatorArgs {
            operation: "add".to_string(),
            a: 5.0,
            b: 3.0,
        };
        let result = server.execute_calculator(args).await;
        let content = &result.content[0];
        if let RawContent::Text(text) = &content.raw {
            assert!(text.text.contains("8"));
        }
    }

    #[tokio::test]
    async fn test_calculator_divide_by_zero() {
        let server = McpServer::new();
        let args = CalculatorArgs {
            operation: "divide".to_string(),
            a: 5.0,
            b: 0.0,
        };
        let result = server.execute_calculator(args).await;
        let content = &result.content[0];
        if let RawContent::Text(text) = &content.raw {
            assert!(text.text.contains("Division by zero"));
        }
    }

    #[tokio::test]
    async fn test_calculator_unknown_operation() {
        let server = McpServer::new();
        let args = CalculatorArgs {
            operation: "unknown".to_string(),
            a: 5.0,
            b: 3.0,
        };
        let result = server.execute_calculator(args).await;
        let content = &result.content[0];
        if let RawContent::Text(text) = &content.raw {
            assert!(text.text.contains("Unknown operation"));
        }
    }

    #[tokio::test]
    async fn test_echo() {
        let server = McpServer::new();
        let result = server.execute_echo("Hello, MCP!".to_string()).await;
        let content = &result.content[0];
        if let RawContent::Text(text) = &content.raw {
            assert_eq!(text.text, "Hello, MCP!");
        }
    }

    #[tokio::test]
    async fn test_server_stats() {
        let server = McpServer::new();
        let result = server.execute_server_stats().await;
        let content = &result.content[0];
        if let RawContent::Text(text) = &content.raw {
            assert!(text.text.contains("ARES MCP Server"));
            assert!(text.text.contains("operation_count"));
        }
    }

    #[tokio::test]
    async fn test_operation_count_increments() {
        let server = McpServer::new();

        // Initial count should be 0
        {
            let count = server.operation_count.lock().await;
            assert_eq!(*count, 0);
        }

        // Perform an operation
        let _ = server.execute_echo("test".to_string()).await;

        // Count should be 1
        {
            let count = server.operation_count.lock().await;
            assert_eq!(*count, 1);
        }

        // Perform another operation
        let args = CalculatorArgs {
            operation: "add".to_string(),
            a: 1.0,
            b: 1.0,
        };
        let _ = server.execute_calculator(args).await;

        // Count should be 2
        {
            let count = server.operation_count.lock().await;
            assert_eq!(*count, 2);
        }
    }

    #[tokio::test]
    async fn test_execute_tool_calculator() {
        let server = McpServer::new();
        let mut args = serde_json::Map::new();
        args.insert("operation".to_string(), json!("multiply"));
        args.insert("a".to_string(), json!(4.0));
        args.insert("b".to_string(), json!(3.0));

        let result = server.execute_tool("calculator", Some(args)).await;
        let content = &result.content[0];
        if let RawContent::Text(text) = &content.raw {
            assert!(text.text.contains("12"));
        }
    }

    #[tokio::test]
    async fn test_execute_tool_unknown() {
        let server = McpServer::new();
        let result = server.execute_tool("nonexistent", None).await;
        let content = &result.content[0];
        if let RawContent::Text(text) = &content.raw {
            assert!(text.text.contains("Unknown tool"));
        }
    }
}
