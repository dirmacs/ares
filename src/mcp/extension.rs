//! MCP tool extension system.
//!
//! Allows extension crates to register additional MCP tools without
//! modifying the core ARES MCP server. The Eruka tools were the first
//! use case, but this is generic for any MCP tool provider.

use async_trait::async_trait;
use rmcp::model::{CallToolResult, Tool};

/// Trait for providing additional MCP tools to the ARES MCP server.
///
/// Extension crates implement this to add domain-specific tools
/// (e.g., Eruka read/write/search, custom knowledge base tools).
#[async_trait]
pub trait McpToolExtension: Send + Sync + 'static {
    /// Return the list of additional tools this extension provides.
    fn tools(&self) -> Vec<Tool>;

    /// Execute a tool call by name. Returns None if this extension
    /// doesn't handle the given tool name.
    async fn execute(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        tenant_id: &str,
    ) -> Option<Result<CallToolResult, String>>;
}

/// No-op extension that provides no additional tools.
pub struct NoOpMcpExtension;

#[async_trait]
impl McpToolExtension for NoOpMcpExtension {
    fn tools(&self) -> Vec<Tool> {
        vec![]
    }

    async fn execute(
        &self,
        _tool_name: &str,
        _arguments: serde_json::Value,
        _tenant_id: &str,
    ) -> Option<Result<CallToolResult, String>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_noop_extension_returns_empty_tools() {
        let ext = NoOpMcpExtension;
        assert!(ext.tools().is_empty(), "NoOp should provide no tools");
    }

    #[tokio::test]
    async fn test_noop_extension_returns_none() {
        let ext = NoOpMcpExtension;
        let result = ext.execute("any_tool", serde_json::json!({}), "tenant_1").await;
        assert!(result.is_none(), "NoOp should not handle any tool");
    }
}
