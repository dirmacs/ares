#!/usr/bin/env python3
from pathlib import Path

TARGET = Path("/opt/ares/crates/ares-mcp/src/registry.rs")
BASE = Path("/opt/ares/crates/ares-mcp/tmp/registry_base.rs")

HEADER = """use std::{collections::HashMap, path::Path, sync::Arc};

use ares_types::types::ToolDefinition;
use async_trait::async_trait;
use rmcp::model::{CallToolResult, Tool};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use super::client::{McpClient, McpServerConfig};
use super::extension::{dispatch_extensions, McpToolExtension};

"""

TOOL_IMPL = r'''
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRegistered { pub name: String }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUnregistered { pub name: String }

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    #[error("tool already registered: {0}")]
    Duplicate(String),
    #[error("tool not found: {0}")]
    NotFound(String),
    #[error("invalid tool schema: {0}")]
    InvalidSchema(String),
}

#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Tool>,
    extensions: Vec<Arc<dyn McpToolExtension>>,
}

impl ToolRegistry {
    pub fn new() -> Self { Self { tools: HashMap::new(), extensions: Vec::new() } }
    pub fn with_builtin_tools() -> Self {
        let mut registry = Self::new();
        for tool in builtin_ares_tools() {
            register_tool(&mut registry.tools, tool).expect("built-in tool names are unique");
        }
        registry
    }
    pub fn register(&mut self, tool: Tool) -> Result<ToolRegistered, RegistryError> {
        register_tool(&mut self.tools, tool)
    }
    pub fn get(&self, name: &str) -> Result<&Tool, RegistryError> { get_tool(&self.tools, name) }
    pub fn unregister(&mut self, name: &str) -> Result<ToolUnregistered, RegistryError> {
        self.tools.remove(name).ok_or_else(|| RegistryError::NotFound(name.to_string()))?;
        Ok(ToolUnregistered { name: name.to_string() })
    }
    pub fn list(&self) -> Vec<Tool> { list_tools(&self.tools, &self.extensions) }
    pub fn register_extension(&mut self, ext: Arc<dyn McpToolExtension>) { self.extensions.push(ext); }
    pub fn remove_extension(&mut self, index: usize) -> bool {
        if index >= self.extensions.len() { return false; }
        self.extensions.remove(index);
        true
    }
    pub fn extensions(&self) -> &[Arc<dyn McpToolExtension>] { &self.extensions }
    pub fn tool_count(&self) -> usize { self.tools.len() }
    pub fn extension_count(&self) -> usize { self.extensions.len() }
}

impl Default for ToolRegistry { fn default() -> Self { Self::new() } }

pub fn register_tool(tools: &mut HashMap<String, Tool>, tool: Tool) -> Result<ToolRegistered, RegistryError> {
    validate_tool_schema(&tool)?;
    let name = tool.name.to_string();
    if tools.contains_key(&name) { return Err(RegistryError::Duplicate(name.clone())); }
    tools.insert(name.clone(), tool);
    Ok(ToolRegistered { name })
}

pub fn get_tool<'a>(tools: &'a HashMap<String, Tool>, name: &str) -> Result<&'a Tool, RegistryError> {
    tools.get(name).ok_or_else(|| RegistryError::NotFound(name.to_string()))
}

pub fn list_tools(tools: &HashMap<String, Tool>, extensions: &[Arc<dyn McpToolExtension>]) -> Vec<Tool> {
    let mut out: Vec<Tool> = tools.values().cloned().collect();
    for ext in extensions { out.extend(ext.tools()); }
    out
}

pub async fn extension_dispatch(
    extensions: &[Arc<dyn McpToolExtension>],
    tool_name: &str,
    arguments: serde_json::Value,
    tenant_id: &str,
) -> Option<Result<CallToolResult, String>> {
    dispatch_extensions(extensions, tool_name, arguments, tenant_id).await
}

pub fn tool_to_definition(tool: &Tool) -> ToolDefinition {
    ToolDefinition {
        name: tool.name.to_string(),
        description: tool.description.clone().map(|d| d.to_string()).unwrap_or_default(),
        parameters: serde_json::to_value(&tool.input_schema).unwrap_or_else(|_| json!({})),
    }
}

pub fn validate_tool_schema(tool: &Tool) -> Result<(), RegistryError> {
    if tool.name.as_ref().trim().is_empty() {
        return Err(RegistryError::InvalidSchema("tool name must not be empty".into()));
    }
    let schema_value = serde_json::to_value(&tool.input_schema)
        .map_err(|e| RegistryError::InvalidSchema(format!("input_schema not serializable: {e}")))?;
    match schema_value.get("type").and_then(|t| t.as_str()) {
        Some("object") => Ok(()),
        Some(other) => Err(RegistryError::InvalidSchema(format!("input_schema type must be object, got {other}"))),
        None => Err(RegistryError::InvalidSchema("input_schema must include type: object".into())),
    }
}

pub fn builtin_ares_tools() -> Vec<Tool> {
    vec![
        Tool { name: "ares_list_agents".into(), description: Some("List all agents available in your ARES account. Returns agent names, descriptions, types, and deployment status.".into()),
            input_schema: serde_json::from_value(json!({"type":"object","properties":{},"required":[]})).unwrap_or_default(),
            annotations: None, icons: None, meta: None, output_schema: None, title: Some("List ARES Agents".into()) },
        Tool { name: "ares_run_agent".into(), description: Some("Run an ARES agent with a message. Specify the agent name and your message. Optionally pass a context_id to continue a conversation.".into()),
            input_schema: serde_json::from_value(json!({"type":"object","properties":{"agent_name":{"type":"string"},"message":{"type":"string"},"context_id":{"type":"string"}},"required":["agent_name","message"]})).unwrap_or_default(),
            annotations: None, icons: None, meta: None, output_schema: None, title: Some("Run ARES Agent".into()) },
        Tool { name: "ares_get_status".into(), description: Some("Check the status of a previous agent run. Pass the context_id from an ares_run_agent call. Returns running/completed/failed status.".into()),
            input_schema: serde_json::from_value(json!({"type":"object","properties":{"context_id":{"type":"string"}},"required":["context_id"]})).unwrap_or_default(),
            annotations: None, icons: None, meta: None, output_schema: None, title: Some("Get Agent Status".into()) },
        Tool { name: "ares_deploy_agent".into(), description: Some("Deploy a new agent to ARES by providing a .toon configuration (TOML format). The agent becomes immediately available for use.".into()),
            input_schema: serde_json::from_value(json!({"type":"object","properties":{"toon_config":{"type":"string"},"name_override":{"type":"string"}},"required":["toon_config"]})).unwrap_or_default(),
            annotations: None, icons: None, meta: None, output_schema: None, title: Some("Deploy Agent".into()) },
        Tool { name: "ares_get_usage".into(), description: Some("Check your ARES account usage statistics and quota. Shows requests made, tokens consumed, and remaining quota for your tier.".into()),
            input_schema: serde_json::from_value(json!({"type":"object","properties":{"from_date":{"type":"string"},"to_date":{"type":"string"}},"required":[]})).unwrap_or_default(),
            annotations: None, icons: None, meta: None, output_schema: None, title: Some("Get Usage Stats".into()) },
    ]
}
'''

NEW_TESTS = Path(__file__).with_name('new_tests_r38.txt')
NEW_TESTS_CONTENT = r'''
    use crate::extension::NoOpMcpExtension;
    use rmcp::model::Content;

    fn sample_tool(name: &str) -> Tool {
        Tool {
            name: std::borrow::Cow::Owned(name.to_string()),
            description: Some(std::borrow::Cow::Owned(format!("{name} tool"))),
            input_schema: serde_json::from_value(json!({"type":"object","properties":{},"required":[]})).unwrap_or_default(),
            annotations: None, icons: None, meta: None, output_schema: None, title: None,
        }
    }

    fn serde_roundtrip<T>(value: &T) -> T
    where T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let j = serde_json::to_string(value).unwrap();
        let p: T = serde_json::from_str(&j).unwrap();
        assert_eq!(*value, p);
        p
    }

    #[test] fn tool_registry_new_is_empty() { let r = ToolRegistry::new(); assert_eq!(r.tool_count(), 0); assert!(r.list().is_empty()); }
    #[test] fn tool_registry_default_matches_new() { assert_eq!(ToolRegistry::default().tool_count(), 0); }
    #[test] fn tool_registry_with_builtin_has_five_unique_tools() {
        let r = ToolRegistry::with_builtin_tools();
        assert_eq!(r.tool_count(), 5);
        let names: Vec<String> = r.list().into_iter().map(|t| t.name.to_string()).collect();
        assert!(names.iter().any(|n| n == "ares_list_agents"));
        assert_eq!(names.len(), names.iter().collect::<std::collections::HashSet<_>>().len());
    }
    #[test] fn register_tool_inserts_and_returns_event() {
        let mut tools = HashMap::new();
        assert_eq!(register_tool(&mut tools, sample_tool("custom")).unwrap().name, "custom");
    }
    #[test] fn register_tool_duplicate_returns_error() {
        let mut r = ToolRegistry::new();
        r.register(sample_tool("dup")).unwrap();
        assert!(matches!(r.register(sample_tool("dup")).unwrap_err(), RegistryError::Duplicate(_)));
    }
    #[test] fn get_tool_returns_reference_when_present() {
        assert_eq!(ToolRegistry::with_builtin_tools().get("ares_list_agents").unwrap().name.as_ref(), "ares_list_agents");
    }
    #[test] fn get_tool_not_found_returns_error() {
        assert!(matches!(ToolRegistry::with_builtin_tools().get("missing").unwrap_err(), RegistryError::NotFound(_)));
    }
    #[test] fn unregister_tool_returns_event() {
        let mut r = ToolRegistry::new();
        r.register(sample_tool("temp")).unwrap();
        assert_eq!(r.unregister("temp").unwrap().name, "temp");
        assert_eq!(r.tool_count(), 0);
    }
    #[test] fn unregister_tool_not_found_returns_error() {
        assert!(matches!(ToolRegistry::new().unregister("ghost").unwrap_err(), RegistryError::NotFound(_)));
    }
    #[test] fn list_tools_includes_extension_tools() {
        struct Ext;
        #[async_trait]
        impl McpToolExtension for Ext {
            fn tools(&self) -> Vec<Tool> { vec![sample_tool("ext_search")] }
            async fn execute(&self, _: &str, _: serde_json::Value, _: &str) -> Option<Result<CallToolResult, String>> { None }
        }
        let mut r = ToolRegistry::with_builtin_tools();
        r.register_extension(Arc::new(Ext));
        assert_eq!(r.list().len(), 6);
    }
    #[test] fn register_and_remove_extension() {
        let mut r = ToolRegistry::new();
        r.register_extension(Arc::new(NoOpMcpExtension));
        assert!(r.remove_extension(0));
        assert!(!r.remove_extension(0));
    }
    #[test] fn validate_tool_schema_rejects_empty_name() {
        assert!(matches!(validate_tool_schema(&sample_tool(" ")).unwrap_err(), RegistryError::InvalidSchema(_)));
    }
    #[test] fn validate_tool_schema_rejects_non_object_type() {
        let mut t = sample_tool("bad");
        t.input_schema = serde_json::from_value(json!({"type":"string"})).unwrap_or_default();
        assert!(matches!(validate_tool_schema(&t).unwrap_err(), RegistryError::InvalidSchema(_)));
    }
    #[test] fn validate_tool_schema_accepts_builtin_tools() { for t in builtin_ares_tools() { validate_tool_schema(&t).unwrap(); } }
    #[test] fn tool_to_definition_maps_fields() {
        let d = tool_to_definition(&sample_tool("mapper"));
        assert_eq!(d.name, "mapper");
        assert_eq!(d.parameters["type"], "object");
    }
    #[test] fn tool_registered_serde_roundtrip() { serde_roundtrip(&ToolRegistered { name: "x".into() }); }
    #[test] fn tool_unregistered_serde_roundtrip() { serde_roundtrip(&ToolUnregistered { name: "y".into() }); }
    #[test] fn tool_definition_from_builtin_serde_roundtrip() {
        let t = builtin_ares_tools().into_iter().find(|x| x.name.as_ref() == "ares_deploy_agent").unwrap();
        let d = tool_to_definition(&t);
        let r: ToolDefinition = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(r.name, "ares_deploy_agent");
    }
    #[test] fn pure_get_tool_helper_matches_registry_get() {
        let mut tools = HashMap::new();
        register_tool(&mut tools, sample_tool("ares_get_status")).unwrap();
        assert_eq!(get_tool(&tools, "ares_get_status").unwrap().name.as_ref(), ToolRegistry::with_builtin_tools().get("ares_get_status").unwrap().name.as_ref());
    }
    #[test] fn pure_list_tools_helper_without_extensions() {
        let mut tools = HashMap::new();
        for t in builtin_ares_tools() { register_tool(&mut tools, t).unwrap(); }
        assert_eq!(list_tools(&tools, &[]).len(), 5);
    }
    #[tokio::test] async fn extension_dispatch_returns_none_for_unknown_tool() {
        assert!(extension_dispatch(ToolRegistry::new().extensions(), "unknown", json!({}), "t").await.is_none());
    }
    #[tokio::test] async fn extension_dispatch_returns_ok_when_extension_handles_tool() {
        struct Echo;
        #[async_trait]
        impl McpToolExtension for Echo {
            fn tools(&self) -> Vec<Tool> { vec![sample_tool("echo_ext")] }
            async fn execute(&self, n: &str, _: serde_json::Value, _: &str) -> Option<Result<CallToolResult, String>> {
                if n == "echo_ext" { Some(Ok(CallToolResult::success(vec![Content::text("ok")]))) } else { None }
            }
        }
        let mut r = ToolRegistry::new();
        r.register_extension(Arc::new(Echo));
        let ok = extension_dispatch(r.extensions(), "echo_ext", json!({}), "t").await.unwrap().unwrap();
        assert!(!ok.is_error.unwrap_or(true));
    }
    #[test] fn register_tool_rejects_invalid_schema_before_duplicate_check() {
        let mut tools = HashMap::new();
        let mut bad = sample_tool("bad");
        bad.input_schema = serde_json::from_value(json!({"type":"array"})).unwrap_or_default();
        assert!(matches!(register_tool(&mut tools, bad).unwrap_err(), RegistryError::InvalidSchema(_)));
        assert!(tools.is_empty());
    }
'''

def main():
    base = BASE.read_text()
    if 'pub struct ToolRegistry' in base:
        print('already applied')
        return
    marker = '#[cfg(test)]'
    mcp = base.split(marker)[0]
    for cut in [
        'use std::{collections::HashMap, path::Path, sync::Arc};\n\nuse super::client::{McpClient, McpServerConfig};\n\n',
        'use std::{collections::HashMap, path::Path, sync::Arc};\n\n',
    ]:
        if mcp.startswith(cut):
            mcp = mcp[len(cut):]
    tests = base[base.index(marker):]
    head, _, _ = tests.rpartition('\n}')
    merged_tests = head + NEW_TESTS_CONTENT + '\n}\n'
    out = HEADER + mcp.strip() + '\n\n' + TOOL_IMPL.strip() + '\n\n' + merged_tests
    TARGET.write_text(out)
    print('wrote', len(out.splitlines()), 'lines')

if __name__ == '__main__':
    main()
