use crate::types::{Result, ToolDefinition};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, args: Value) -> Result<Value>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Create a new registry with default tools (web search, page fetch, calculator)
    pub fn with_default_tools() -> Self {
        let mut registry = Self::new();
        
        // Register daedra-powered search and fetch tools
        registry.register(Arc::new(crate::tools::search::SearchTool::new()));
        registry.register(Arc::new(crate::tools::search::FetchPageTool::new()));
        
        // Register calculator tool
        registry.register(Arc::new(crate::tools::calculator::Calculator));
        
        registry
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|tool| ToolDefinition {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                parameters: tool.parameters_schema(),
            })
            .collect()
    }

    pub async fn execute(&self, name: &str, args: Value) -> Result<Value> {
        if let Some(tool) = self.tools.get(name) {
            tool.execute(args).await
        } else {
            Err(crate::types::AppError::NotFound(format!(
                "Tool not found: {}",
                name
            )))
        }
    }
    
    /// Get a list of all registered tool names
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
    
    /// Check if a tool is registered
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = ToolRegistry::new();
        assert_eq!(registry.tool_names().len(), 0);
    }

    #[test]
    fn test_registry_with_default_tools() {
        let registry = ToolRegistry::with_default_tools();
        let tools = registry.tool_names();
        
        // Should have web_search, fetch_page, and calculator
        assert!(tools.len() >= 3);
        assert!(registry.has_tool("web_search"));
        assert!(registry.has_tool("fetch_page"));
        assert!(registry.has_tool("calculator"));
    }

    #[test]
    fn test_get_tool_definitions() {
        let registry = ToolRegistry::with_default_tools();
        let definitions = registry.get_tool_definitions();
        
        assert!(definitions.len() >= 3);
        
        // Verify each definition has required fields
        for def in &definitions {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
            assert!(def.parameters.is_object());
        }
    }

    #[tokio::test]
    async fn test_calculator_execution() {
        let registry = ToolRegistry::with_default_tools();
        
        let args = serde_json::json!({
            "operation": "add",
            "a": 5.0,
            "b": 3.0
        });
        
        let result = registry.execute("calculator", args).await;
        assert!(result.is_ok());
        
        let value = result.unwrap();
        assert_eq!(value["result"], 8.0);
    }

    #[tokio::test]
    async fn test_nonexistent_tool() {
        let registry = ToolRegistry::with_default_tools();
        
        let result = registry.execute("nonexistent_tool", serde_json::json!({})).await;
        assert!(result.is_err());
    }
}
