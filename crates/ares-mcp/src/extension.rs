//! MCP tool extension system.
//!
//! Allows extension crates to register additional MCP tools without
//! modifying the core ARES MCP server. The Eruka tools were the first
//! use case, but this is generic for any MCP tool provider.

use async_trait::async_trait;
use rmcp::model::{CallToolResult, Tool};
use std::sync::Arc;

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

/// Dispatch a tool call to registered extensions in registration order.
///
/// Returns `Some(Ok)` or `Some(Err)` when the first extension claims the tool.
/// Returns `None` when no extension handles the name (caller should fall back).
pub async fn dispatch_extensions(
    extensions: &[Arc<dyn McpToolExtension>],
    tool_name: &str,
    arguments: serde_json::Value,
    tenant_id: &str,
) -> Option<Result<CallToolResult, String>> {
    for ext in extensions {
        if let Some(result) = ext.execute(tool_name, arguments.clone(), tenant_id).await {
            return Some(result);
        }
    }
    None
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
    use rmcp::model::ContentBlock as RmcpContent;
    use serde_json::json;

    fn test_tool(name: &str) -> Tool {
        let input_schema: rmcp::model::JsonObject = serde_json::from_value(json!({
            "type": "object",
            "properties": {},
            "required": []
        }))
        .unwrap_or_default();
        Tool::new(name.to_string(), "test extension tool", input_schema)
            .with_title(format!("Test {name}"))
    }

    struct MatchingExtension {
        tool_name: &'static str,
        result_text: &'static str,
    }

    #[async_trait]
    impl McpToolExtension for MatchingExtension {
        fn tools(&self) -> Vec<Tool> {
            vec![test_tool(self.tool_name)]
        }

        async fn execute(
            &self,
            tool_name: &str,
            _arguments: serde_json::Value,
            _tenant_id: &str,
        ) -> Option<Result<CallToolResult, String>> {
            if tool_name == self.tool_name {
                Some(Ok(CallToolResult::success(vec![RmcpContent::text(
                    self.result_text,
                )])))
            } else {
                None
            }
        }
    }

    struct FailingExtension {
        tool_name: &'static str,
        error_message: &'static str,
    }

    #[async_trait]
    impl McpToolExtension for FailingExtension {
        fn tools(&self) -> Vec<Tool> {
            vec![]
        }

        async fn execute(
            &self,
            tool_name: &str,
            _arguments: serde_json::Value,
            _tenant_id: &str,
        ) -> Option<Result<CallToolResult, String>> {
            if tool_name == self.tool_name {
                Some(Err(self.error_message.into()))
            } else {
                None
            }
        }
    }

    struct TenantCapturingExtension;

    #[async_trait]
    impl McpToolExtension for TenantCapturingExtension {
        fn tools(&self) -> Vec<Tool> {
            vec![]
        }

        async fn execute(
            &self,
            tool_name: &str,
            _arguments: serde_json::Value,
            tenant_id: &str,
        ) -> Option<Result<CallToolResult, String>> {
            if tool_name == "tenant_echo" {
                Some(Ok(CallToolResult::success(vec![RmcpContent::text(
                    tenant_id,
                )])))
            } else {
                None
            }
        }
    }

    struct PassThroughExtension;

    #[async_trait]
    impl McpToolExtension for PassThroughExtension {
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

    fn sample_arguments() -> serde_json::Value {
        serde_json::json!({ "key": "value" })
    }

    #[test]
    fn test_noop_extension_returns_empty_tools() {
        let ext = NoOpMcpExtension;
        assert!(ext.tools().is_empty(), "NoOp should provide no tools");
    }

    #[tokio::test]
    async fn test_noop_extension_returns_none() {
        let ext = NoOpMcpExtension;
        let result = ext
            .execute("any_tool", serde_json::json!({}), "tenant_1")
            .await;
        assert!(result.is_none(), "NoOp should not handle any tool");
    }

    #[test]
    fn test_extension_matching_exposes_tool_metadata() {
        let ext = MatchingExtension {
            tool_name: "custom_tool",
            result_text: "ok",
        };
        let tools = ext.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "custom_tool");
    }

    #[tokio::test]
    async fn test_extension_matching_handles_only_registered_name() {
        let ext = MatchingExtension {
            tool_name: "custom_tool",
            result_text: "matched",
        };

        let matched = ext
            .execute("custom_tool", sample_arguments(), "tenant_1")
            .await
            .expect("should handle registered tool")
            .expect("should succeed");
        let text = matched
            .content
            .first()
            .unwrap()
            .as_text()
            .unwrap()
            .text
            .as_str();
        assert_eq!(text, "matched");

        let unmatched = ext
            .execute("other_tool", sample_arguments(), "tenant_1")
            .await;
        assert!(unmatched.is_none(), "should not handle unknown tool names");
    }

    #[tokio::test]
    async fn test_dispatch_no_extensions_returns_none() {
        let extensions: Vec<Arc<dyn McpToolExtension>> = vec![];
        let result =
            dispatch_extensions(&extensions, "custom_tool", sample_arguments(), "tenant_1").await;
        assert!(result.is_none(), "empty registry should fall through");
    }

    #[tokio::test]
    async fn test_dispatch_matching_extension_success() {
        let extensions: Vec<Arc<dyn McpToolExtension>> = vec![Arc::new(MatchingExtension {
            tool_name: "custom_tool",
            result_text: "custom result",
        })];
        let result =
            dispatch_extensions(&extensions, "custom_tool", sample_arguments(), "tenant_1")
                .await
                .expect("extension should handle tool")
                .expect("dispatch should succeed");
        let text = result
            .content
            .first()
            .unwrap()
            .as_text()
            .unwrap()
            .text
            .as_str();
        assert_eq!(text, "custom result");
    }

    #[tokio::test]
    async fn test_dispatch_extension_error_path() {
        let extensions: Vec<Arc<dyn McpToolExtension>> = vec![Arc::new(FailingExtension {
            tool_name: "failing_tool",
            error_message: "extension failure",
        })];
        let result =
            dispatch_extensions(&extensions, "failing_tool", sample_arguments(), "tenant_1")
                .await
                .expect("extension should claim tool");
        let err = result.expect_err("dispatch should propagate extension error");
        assert_eq!(err, "extension failure");
    }

    #[tokio::test]
    async fn test_dispatch_fallback_when_no_extension_matches() {
        let extensions: Vec<Arc<dyn McpToolExtension>> =
            vec![Arc::new(PassThroughExtension), Arc::new(NoOpMcpExtension)];
        let result =
            dispatch_extensions(&extensions, "unknown_tool", sample_arguments(), "tenant_1").await;
        assert!(
            result.is_none(),
            "unhandled tools should fall through to caller fallback"
        );
    }

    #[tokio::test]
    async fn test_dispatch_first_matching_extension_wins() {
        let extensions: Vec<Arc<dyn McpToolExtension>> = vec![
            Arc::new(MatchingExtension {
                tool_name: "custom_tool",
                result_text: "first",
            }),
            Arc::new(MatchingExtension {
                tool_name: "custom_tool",
                result_text: "second",
            }),
        ];
        let result =
            dispatch_extensions(&extensions, "custom_tool", sample_arguments(), "tenant_1")
                .await
                .expect("extension should handle tool")
                .expect("dispatch should succeed");
        let text = result
            .content
            .first()
            .unwrap()
            .as_text()
            .unwrap()
            .text
            .as_str();
        assert_eq!(text, "first", "first registered extension should win");
    }

    #[tokio::test]
    async fn test_dispatch_skips_non_matching_extension() {
        let extensions: Vec<Arc<dyn McpToolExtension>> = vec![
            Arc::new(PassThroughExtension),
            Arc::new(MatchingExtension {
                tool_name: "custom_tool",
                result_text: "from second extension",
            }),
        ];
        let result =
            dispatch_extensions(&extensions, "custom_tool", sample_arguments(), "tenant_1")
                .await
                .expect("later extension should handle tool")
                .expect("dispatch should succeed");
        let text = result
            .content
            .first()
            .unwrap()
            .as_text()
            .unwrap()
            .text
            .as_str();
        assert_eq!(text, "from second extension");
    }

    #[tokio::test]
    async fn test_dispatch_passes_tenant_id_to_extension() {
        let extensions: Vec<Arc<dyn McpToolExtension>> = vec![Arc::new(TenantCapturingExtension)];
        let result =
            dispatch_extensions(&extensions, "tenant_echo", sample_arguments(), "tenant_42")
                .await
                .expect("extension should handle tool")
                .expect("dispatch should succeed");
        let text = result
            .content
            .first()
            .unwrap()
            .as_text()
            .unwrap()
            .text
            .as_str();
        assert_eq!(text, "tenant_42");
    }
}
