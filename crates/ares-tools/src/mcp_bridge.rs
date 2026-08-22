//! MCP Bridge tools for A.R.E.S.
//!
//! These tools bridge A.R.E.S. to external MCP servers, allowing tools to query
//! context, gaps, and completeness from MCP-compatible servers.

use crate::registry::{Tool, ToolRegistry};
#[cfg(test)]
use crate::ToolConfig;
use ares_mcp::client::{McpClient, McpServerConfig};
use ares_types::types::ToolDefinition;
use ares_types::{AppError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

// =============================================================================
// Pure types and helpers (R45)
// =============================================================================

/// Serializable metadata for a single MCP bridge tool exposed to agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MCPTool {
    pub name: String,
    pub description: String,
    pub parameters_schema: Value,
}

impl fmt::Display for MCPTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.name, self.description)
    }
}

/// Per-tool enablement for MCP bridge tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MCPToolConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl Default for MCPToolConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Errors surfaced by pure MCP bridge helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpBridgeError {
    McpServerError(String),
    ToolNotFound(String),
    InvalidParams(String),
}

impl McpBridgeError {
    pub fn mcp_server_error(message: impl Into<String>) -> Self {
        Self::McpServerError(message.into())
    }

    pub fn tool_not_found(name: impl Into<String>) -> Self {
        Self::ToolNotFound(name.into())
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::InvalidParams(message.into())
    }
}

impl fmt::Display for McpBridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::McpServerError(msg) => write!(f, "MCP server error: {msg}"),
            Self::ToolNotFound(name) => write!(f, "tool not found: {name}"),
            Self::InvalidParams(msg) => write!(f, "invalid params: {msg}"),
        }
    }
}

impl std::error::Error for McpBridgeError {}

/// Names of all MCP bridge tools registered by [`register_mcp_tools`].
pub const MCP_BRIDGE_TOOL_NAMES: &[&str] = &[
    "mcp_get_context",
    "mcp_write_context",
    "mcp_search_context",
    "mcp_get_completeness",
    "mcp_get_gaps",
    "mcp_detect_gaps",
];

/// Static catalog of MCP bridge tool metadata used for definitions and validation.
pub fn mcp_bridge_tool_catalog() -> Vec<MCPTool> {
    vec![
        MCPTool {
            name: "mcp_get_context".into(),
            description: "Get context from an MCP server by path".into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The path to retrieve context from"
                    }
                },
                "required": ["path"]
            }),
        },
        MCPTool {
            name: "mcp_write_context".into(),
            description: "Write context to an MCP server at a specific path".into(),
            parameters_schema: json!({
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
            }),
        },
        MCPTool {
            name: "mcp_search_context".into(),
            description: "Search context in an MCP server with optional scope and max results".into(),
            parameters_schema: json!({
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
            }),
        },
        MCPTool {
            name: "mcp_get_completeness".into(),
            description: "Get completeness metrics from an MCP server for a scope".into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "scope": {
                        "type": "string",
                        "description": "Optional scope to get completeness for (defaults to '*')",
                        "default": "*"
                    }
                }
            }),
        },
        MCPTool {
            name: "mcp_get_gaps".into(),
            description: "Get gaps from an MCP server filtered by status and category".into(),
            parameters_schema: json!({
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
            }),
        },
        MCPTool {
            name: "mcp_detect_gaps".into(),
            description: "Detect gaps in an MCP server optionally filtered by category".into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "category": {
                        "type": "string",
                        "description": "Optional category to detect gaps for"
                    }
                }
            }),
        },
    ]
}


fn tool_description(name: &str) -> &'static str {
    match name {
        "mcp_get_context" => "Get context from an MCP server by path",
        "mcp_write_context" => "Write context to an MCP server at a specific path",
        "mcp_search_context" => "Search context in an MCP server with optional scope and max results",
        "mcp_get_completeness" => "Get completeness metrics from an MCP server for a scope",
        "mcp_get_gaps" => "Get gaps from an MCP server filtered by status and category",
        "mcp_detect_gaps" => "Detect gaps in an MCP server optionally filtered by category",
        _ => "MCP bridge tool",
    }
}


fn catalog_tool(name: &str) -> Option<MCPTool> {
    mcp_bridge_tool_catalog()
        .into_iter()
        .find(|tool| tool.name == name)
}

fn require_str_field(args: &Value, field: &str) -> std::result::Result<(), McpBridgeError> {
    args.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|_| ())
        .ok_or_else(|| McpBridgeError::invalid_params(format!("{field} is required")))
}

/// Validate tool arguments for a named MCP bridge tool before dispatch.
pub fn validate_mcp_call_params(tool_name: &str, args: &Value) -> std::result::Result<(), McpBridgeError> {
    if catalog_tool(tool_name).is_none() {
        return Err(McpBridgeError::tool_not_found(tool_name));
    }

    match tool_name {
        "mcp_get_context" => require_str_field(args, "path"),
        "mcp_write_context" => {
            require_str_field(args, "path")?;
            require_str_field(args, "value")
        }
        "mcp_search_context" => require_str_field(args, "query"),
        _ => Ok(()),
    }
}

/// Parse a JSON-RPC MCP response envelope or bare JSON payload.
pub fn parse_mcp_response(payload: &Value) -> std::result::Result<Value, McpBridgeError> {
    if let Some(error) = payload.get("error") {
        if let Some(message) = error.get("message").and_then(Value::as_str) {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32000);
            return Err(McpBridgeError::mcp_server_error(format!("{code}: {message}")));
        }
        return Err(McpBridgeError::mcp_server_error(error.to_string()));
    }

    if let Some(result) = payload.get("result") {
        return Ok(result.clone());
    }

    if payload.get("jsonrpc").is_some() {
        return Err(McpBridgeError::mcp_server_error(
            "MCP response missing result and error",
        ));
    }

    Ok(payload.clone())
}

/// Validate call parameters and parse the MCP JSON-RPC response body.
pub fn execute_mcp_call(
    tool_name: &str,
    args: &Value,
    response: &Value,
) -> std::result::Result<Value, McpBridgeError> {
    validate_mcp_call_params(tool_name, args)?;
    parse_mcp_response(response)
}

/// Build LLM tool definitions from catalog entries, honoring per-tool enablement.
pub fn build_tool_definitions(
    tools: &[MCPTool],
    configs: &HashMap<String, MCPToolConfig>,
) -> Vec<ToolDefinition> {
    tools
        .iter()
        .filter(|tool| configs.get(&tool.name).map(|cfg| cfg.enabled).unwrap_or(true))
        .map(|tool| ToolDefinition {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters_schema.clone(),
        })
        .collect()
}

/// Build the MCP server client configuration for a named bridge client.
pub fn mcp_server_config_for_client(client_name: &str) -> McpServerConfig {
    McpServerConfig {
        name: client_name.to_string(),
        enabled: true,
        command: None,
        args: None,
        timeout_secs: None,
        endpoint: Some("http://127.0.0.1:9999".to_string()),
        transport: Some("http".to_string()),
        api_key: None,
    }
}

/// Register MCP bridge tool implementations for `client_name`.
pub fn register_mcp_tools(registry: &mut ToolRegistry, client_name: &str) {
    let config = mcp_server_config_for_client(client_name);

    registry.register(Arc::new(McpGetContext::new(McpClient::new(config.clone()))));
    registry.register(Arc::new(McpWriteContext::new(McpClient::new(config.clone()))));
    registry.register(Arc::new(McpSearchContext::new(McpClient::new(config.clone()))));
    registry.register(Arc::new(McpGetCompleteness::new(McpClient::new(config.clone()))));
    registry.register(Arc::new(McpGetGaps::new(McpClient::new(config.clone()))));
    registry.register(Arc::new(McpDetectGaps::new(McpClient::new(config.clone()))));
}

fn map_mcp_client_error(err: impl ToString) -> AppError {
    AppError::External(err.to_string())
}

// =============================================================================
// Tool implementations
// =============================================================================

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
        tool_description(self.name())
    }

    fn parameters_schema(&self) -> Value {
        catalog_tool(self.name())
            .map(|tool| tool.parameters_schema)
            .unwrap_or_else(|| json!({"type": "object"}))
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        validate_mcp_call_params(self.name(), &args)
            .map_err(|e| AppError::InvalidInput(e.to_string()))?;
        let path = args["path"].as_str().expect("validated");

        self.client
            .get_context(path)
            .await
            .map_err(map_mcp_client_error)
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
        tool_description(self.name())
    }

    fn parameters_schema(&self) -> Value {
        catalog_tool(self.name())
            .map(|tool| tool.parameters_schema)
            .unwrap_or_else(|| json!({"type": "object"}))
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        validate_mcp_call_params(self.name(), &args)
            .map_err(|e| AppError::InvalidInput(e.to_string()))?;
        let path = args["path"].as_str().expect("validated");
        let value = args["value"].as_str().expect("validated");

        self.client
            .write_context(path, value)
            .await
            .map_err(map_mcp_client_error)
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
        tool_description(self.name())
    }

    fn parameters_schema(&self) -> Value {
        catalog_tool(self.name())
            .map(|tool| tool.parameters_schema)
            .unwrap_or_else(|| json!({"type": "object"}))
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        validate_mcp_call_params(self.name(), &args)
            .map_err(|e| AppError::InvalidInput(e.to_string()))?;
        let query = args["query"].as_str().expect("validated");

        let scope = args["scope"].as_str();
        let max_results = args["max_results"].as_i64().map(|m| m as usize);

        self.client
            .search_context(query, scope, max_results)
            .await
            .map_err(map_mcp_client_error)
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
        tool_description(self.name())
    }

    fn parameters_schema(&self) -> Value {
        catalog_tool(self.name())
            .map(|tool| tool.parameters_schema)
            .unwrap_or_else(|| json!({"type": "object"}))
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let _ = validate_mcp_call_params(self.name(), &args);
        let scope = args["scope"].as_str();

        self.client
            .get_completeness(scope)
            .await
            .map_err(map_mcp_client_error)
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
        tool_description(self.name())
    }

    fn parameters_schema(&self) -> Value {
        catalog_tool(self.name())
            .map(|tool| tool.parameters_schema)
            .unwrap_or_else(|| json!({"type": "object"}))
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let _ = validate_mcp_call_params(self.name(), &args);
        let status = args["status"].as_str();
        let category = args["category"].as_str();

        self.client
            .get_gaps(status, category)
            .await
            .map_err(map_mcp_client_error)
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
        tool_description(self.name())
    }

    fn parameters_schema(&self) -> Value {
        catalog_tool(self.name())
            .map(|tool| tool.parameters_schema)
            .unwrap_or_else(|| json!({"type": "object"}))
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let _ = validate_mcp_call_params(self.name(), &args);
        let category = args["category"].as_str();

        self.client
            .detect_gaps(category)
            .await
            .map_err(map_mcp_client_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::AppError;

    fn test_client() -> McpClient {
        McpClient::new(mcp_server_config_for_client("test_client"))
    }

    fn disabled_tool_config() -> ToolConfig {
        ToolConfig {
            enabled: false,
            description: None,
            timeout_secs: 30,
            extra: HashMap::new(),
        }
    }

    // ===============================
    // MCPTool / MCPToolConfig serde
    // ===============================

    #[test]
    fn mcp_tool_serde_roundtrip() {
        let tool = mcp_bridge_tool_catalog()[0].clone();
        let json = serde_json::to_string(&tool).expect("serialize");
        let back: MCPTool = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, tool);
    }

    #[test]
    fn mcp_tool_config_serde_roundtrip() {
        let cfg = MCPToolConfig { enabled: false };
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: MCPToolConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, cfg);
    }

    #[test]
    fn mcp_tool_config_defaults_enabled() {
        let cfg: MCPToolConfig = serde_json::from_str("{}").expect("deserialize");
        assert!(cfg.enabled);
    }

    #[test]
    fn mcp_tool_config_default_impl_matches_serde_default() {
        assert_eq!(MCPToolConfig::default(), MCPToolConfig { enabled: true });
    }

    #[test]
    fn mcp_server_config_for_client_sets_name_and_endpoint() {
        let cfg = mcp_server_config_for_client("bridge-a");
        assert_eq!(cfg.name, "bridge-a");
        assert!(cfg.endpoint.as_deref().unwrap().contains("127.0.0.1"));
        assert!(cfg.enabled);
    }

    #[test]
    fn mcp_tool_catalog_covers_all_registered_names() {
        let catalog = mcp_bridge_tool_catalog();
        let names: Vec<_> = catalog.iter().map(|tool| tool.name.as_str()).collect();
        for expected in MCP_BRIDGE_TOOL_NAMES {
            assert!(names.contains(expected), "missing catalog entry for {expected}");
        }
    }

    // ===============================
    // register_mcp_tools
    // ===============================

    #[test]
    fn register_mcp_tools_registers_all_tools() {
        let mut registry = ToolRegistry::new();
        register_mcp_tools(&mut registry, "test_client");

        for name in MCP_BRIDGE_TOOL_NAMES {
            assert!(registry.has_tool(name), "missing tool {name}");
        }
    }

    #[test]
    fn register_mcp_tools_count_matches_catalog() {
        let mut registry = ToolRegistry::new();
        register_mcp_tools(&mut registry, "client");
        assert_eq!(registry.enabled_tool_names().len(), MCP_BRIDGE_TOOL_NAMES.len());
    }

    // ===============================
    // build_tool_definitions / registry definitions
    // ===============================

    #[test]
    fn build_tool_definitions_returns_enabled_only() {
        let catalog = mcp_bridge_tool_catalog();
        let mut configs = HashMap::new();
        configs.insert(
            "mcp_get_context".into(),
            MCPToolConfig { enabled: false },
        );
        configs.insert("mcp_write_context".into(), MCPToolConfig { enabled: true });

        let defs = build_tool_definitions(&catalog, &configs);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();

        assert!(!names.contains(&"mcp_get_context"));
        assert!(names.contains(&"mcp_write_context"));
        assert_eq!(defs.len(), catalog.len() - 1);
    }

    #[test]
    fn build_tool_definitions_defaults_missing_config_to_enabled() {
        let catalog = mcp_bridge_tool_catalog();
        let defs = build_tool_definitions(&catalog, &HashMap::new());
        assert_eq!(defs.len(), catalog.len());
    }

    #[test]
    fn build_tool_definitions_preserves_parameter_schema() {
        let catalog = mcp_bridge_tool_catalog();
        let defs = build_tool_definitions(&catalog, &HashMap::new());
        let search = defs
            .iter()
            .find(|d| d.name == "mcp_search_context")
            .expect("search tool");
        assert!(search.parameters["properties"]["query"].is_object());
    }

    #[test]
    fn get_tool_definitions_returns_enabled_tools_only() {
        let mut registry = ToolRegistry::new();
        register_mcp_tools(&mut registry, "test");
        registry.set_config("mcp_get_gaps", disabled_tool_config());

        let defs = registry.get_tool_definitions();
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();

        assert!(!names.contains(&"mcp_get_gaps"));
        assert_eq!(defs.len(), MCP_BRIDGE_TOOL_NAMES.len() - 1);
    }

    #[test]
    fn get_tool_definitions_uses_tool_descriptions() {
        let mut registry = ToolRegistry::new();
        register_mcp_tools(&mut registry, "test");

        let defs = registry.get_tool_definitions();
        let write = defs
            .iter()
            .find(|d| d.name == "mcp_write_context")
            .expect("write tool");

        assert_eq!(
            write.description,
            "Write context to an MCP server at a specific path"
        );
    }

    // ===============================
    // parse_mcp_response
    // ===============================

    #[test]
    fn parse_mcp_response_success_result() {
        let payload = json!({"jsonrpc": "2.0", "id": 1, "result": {"items": [1]}});
        let value = parse_mcp_response(&payload).expect("success");
        assert_eq!(value["items"][0], 1);
    }

    #[test]
    fn parse_mcp_response_error_object() {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32600, "message": "Invalid Request"}
        });
        let err = parse_mcp_response(&payload).unwrap_err();
        assert!(matches!(err, McpBridgeError::McpServerError(_)));
        assert!(err.to_string().contains("Invalid Request"));
    }

    #[test]
    fn parse_mcp_response_bare_object_without_envelope() {
        let payload = json!({"status": "ok"});
        let value = parse_mcp_response(&payload).expect("bare payload");
        assert_eq!(value["status"], "ok");
    }

    #[test]
    fn parse_mcp_response_missing_result_and_error() {
        let payload = json!({"jsonrpc": "2.0", "id": 9});
        let err = parse_mcp_response(&payload).unwrap_err();
        assert!(matches!(err, McpBridgeError::McpServerError(_)));
    }

    #[test]
    fn parse_mcp_response_null_result() {
        let payload = json!({"jsonrpc": "2.0", "result": null});
        let value = parse_mcp_response(&payload).expect("null result");
        assert!(value.is_null());
    }

    // ===============================
    // execute_mcp_call / validation
    // ===============================

    #[test]
    fn execute_mcp_call_success_path() {
        let args = json!({"path": "/ctx"});
        let response = json!({"jsonrpc": "2.0", "result": {"content": "hi"}});
        let value = execute_mcp_call("mcp_get_context", &args, &response).expect("ok");
        assert_eq!(value["content"], "hi");
    }

    #[test]
    fn execute_mcp_call_invalid_params() {
        let err = execute_mcp_call("mcp_get_context", &json!({}), &json!({"result": {}}))
            .unwrap_err();
        assert!(matches!(err, McpBridgeError::InvalidParams(_)));
    }

    #[test]
    fn execute_mcp_call_tool_not_found() {
        let err = execute_mcp_call("missing_tool", &json!({}), &json!({"result": {}}))
            .unwrap_err();
        assert!(matches!(err, McpBridgeError::ToolNotFound(_)));
    }

    #[test]
    fn execute_mcp_call_propagates_server_error() {
        let response = json!({"error": {"code": -32000, "message": "boom"}});
        let err =
            execute_mcp_call("mcp_search_context", &json!({"query": "x"}), &response).unwrap_err();
        assert!(matches!(err, McpBridgeError::McpServerError(_)));
    }

    #[test]
    fn validate_mcp_write_context_requires_value() {
        let err = validate_mcp_call_params("mcp_write_context", &json!({"path": "/a"}))
            .unwrap_err();
        assert!(matches!(err, McpBridgeError::InvalidParams(_)));
    }

    #[test]
    fn validate_mcp_get_gaps_accepts_empty_object() {
        assert!(validate_mcp_call_params("mcp_get_gaps", &json!({})).is_ok());
    }

    #[test]
    fn parse_mcp_response_non_object_error_value() {
        let err = parse_mcp_response(&json!({"error": "transport failed"})).unwrap_err();
        assert!(matches!(err, McpBridgeError::McpServerError(_)));
    }

    // ===============================
    // Tool signatures (names, descriptions, schemas)
    // ===============================

    #[test]
    fn mcp_get_gaps_signature() {
        let tool = McpGetGaps::new(test_client());
        assert_eq!(tool.name(), "mcp_get_gaps");
        assert_eq!(
            tool.description(),
            "Get gaps from an MCP server filtered by status and category"
        );
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["status"].is_object());
        assert!(schema["properties"]["category"].is_object());
    }

    #[test]
    fn mcp_get_completeness_signature() {
        let tool = McpGetCompleteness::new(test_client());
        assert_eq!(tool.name(), "mcp_get_completeness");
        assert_eq!(
            tool.description(),
            "Get completeness metrics from an MCP server for a scope"
        );
        assert_eq!(tool.parameters_schema()["properties"]["scope"]["default"], "*");
    }

    #[test]
    fn mcp_search_context_signature() {
        let tool = McpSearchContext::new(test_client());
        assert_eq!(tool.name(), "mcp_search_context");
        assert!(tool.description().contains("scope"));
        let schema = tool.parameters_schema();
        assert!(schema["required"].as_array().unwrap().contains(&json!("query")));
    }

    #[test]
    fn mcp_write_context_signature() {
        let tool = McpWriteContext::new(test_client());
        assert_eq!(tool.name(), "mcp_write_context");
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("path")));
        assert!(required.contains(&json!("value")));
    }

    #[test]
    fn mcp_get_context_signature() {
        let tool = McpGetContext::new(test_client());
        assert_eq!(tool.name(), "mcp_get_context");
        assert_eq!(tool.parameters_schema()["properties"]["path"]["type"], "string");
    }

    #[test]
    fn mcp_detect_gaps_signature() {
        let tool = McpDetectGaps::new(test_client());
        assert_eq!(tool.name(), "mcp_detect_gaps");
        assert!(tool.parameters_schema()["properties"]["category"].is_object());
    }

    // ===============================
    // Error variants + Display / Debug / Clone
    // ===============================

    #[test]
    fn mcp_bridge_error_variants_display() {
        assert!(McpBridgeError::mcp_server_error("down")
            .to_string()
            .contains("MCP server error"));
        assert!(McpBridgeError::tool_not_found("x")
            .to_string()
            .contains("tool not found"));
        assert!(McpBridgeError::invalid_params("bad")
            .to_string()
            .contains("invalid params"));
    }

    #[test]
    fn mcp_tool_display_shows_description() {
        let tool = &mcp_bridge_tool_catalog()[1];
        let rendered = format!("{tool}");
        assert!(rendered.contains("mcp_write_context"));
        assert!(rendered.contains("Write context"));
    }

    #[test]
    fn mcp_tool_debug_clone() {
        let tool = mcp_bridge_tool_catalog()[0].clone();
        let cloned = tool.clone();
        assert_eq!(tool, cloned);
        assert!(format!("{tool:?}").contains("mcp_get_context"));
    }

    #[test]
    fn mcp_tool_config_debug_clone() {
        let cfg = MCPToolConfig { enabled: false };
        let cloned = cfg.clone();
        assert_eq!(cfg, cloned);
        assert!(format!("{cfg:?}").contains("enabled"));
    }

    #[test]
    fn mcp_bridge_error_debug_clone() {
        let err = McpBridgeError::tool_not_found("nope");
        let cloned = err.clone();
        assert_eq!(err, cloned);
        assert!(format!("{err:?}").contains("ToolNotFound"));
    }

    // ===============================
    // Async execute integration
    // ===============================

    #[tokio::test]
    async fn mcp_get_context_execute_missing_path() {
        let tool = McpGetContext::new(test_client());
        let result = tool.execute(json!({})).await;
        assert!(matches!(result, Err(AppError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn mcp_write_context_execute_missing_path() {
        let tool = McpWriteContext::new(test_client());
        let result = tool.execute(json!({"value": "test"})).await;
        assert!(matches!(result, Err(AppError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn mcp_search_context_execute_missing_query() {
        let tool = McpSearchContext::new(test_client());
        let result = tool.execute(json!({})).await;
        assert!(matches!(result, Err(AppError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn mcp_get_completeness_execute_returns_external_error() {
        let tool = McpGetCompleteness::new(test_client());
        let result = tool.execute(json!({})).await;
        assert!(matches!(result, Err(AppError::External(_))));
    }

    #[tokio::test]
    async fn mcp_get_gaps_execute_returns_external_error() {
        let tool = McpGetGaps::new(test_client());
        let result = tool.execute(json!({})).await;
        assert!(matches!(result, Err(AppError::External(_))));
    }

    #[tokio::test]
    async fn mcp_detect_gaps_execute_returns_external_error() {
        let tool = McpDetectGaps::new(test_client());
        let result = tool.execute(json!({})).await;
        assert!(matches!(result, Err(AppError::External(_))));
    }
}
