use crate::types::{Result, ToolDefinition};
use crate::utils::toml_config::{AresConfig, ToolConfig};
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

/// Registry for managing tools with configuration support
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    configs: HashMap<String, ToolConfig>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            configs: HashMap::new(),
        }
    }

    /// Create a tool registry with configurations from TOML
    pub fn with_config(config: &AresConfig) -> Self {
        Self {
            tools: HashMap::new(),
            configs: config.tools.clone(),
        }
    }

    /// Register a tool
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Register a tool with its configuration
    pub fn register_with_config(&mut self, tool: Arc<dyn Tool>, config: ToolConfig) {
        let name = tool.name().to_string();
        self.tools.insert(name.clone(), tool);
        self.configs.insert(name, config);
    }

    /// Set tool configuration
    pub fn set_config(&mut self, name: &str, config: ToolConfig) {
        self.configs.insert(name.to_string(), config);
    }

    /// Get tool configuration
    pub fn get_config(&self, name: &str) -> Option<&ToolConfig> {
        self.configs.get(name)
    }

    /// Check if a tool is enabled
    pub fn is_enabled(&self, name: &str) -> bool {
        self.configs
            .get(name)
            .map(|c| c.enabled)
            .unwrap_or(true) // Default to enabled if no config
    }

    /// Get timeout for a tool
    pub fn get_timeout(&self, name: &str) -> u64 {
        self.configs
            .get(name)
            .map(|c| c.timeout_secs)
            .unwrap_or(30) // Default 30 seconds
    }

    /// Get all tool definitions (only enabled tools)
    pub fn get_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .filter(|tool| self.is_enabled(tool.name()))
            .map(|tool| {
                let description = self
                    .get_config(tool.name())
                    .and_then(|c| c.description.clone())
                    .unwrap_or_else(|| tool.description().to_string());

                ToolDefinition {
                    name: tool.name().to_string(),
                    description,
                    parameters: tool.parameters_schema(),
                }
            })
            .collect()
    }

    /// Get tool definitions for specific tool names (only enabled)
    pub fn get_tool_definitions_for(&self, names: &[&str]) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .filter(|tool| names.contains(&tool.name()) && self.is_enabled(tool.name()))
            .map(|tool| {
                let description = self
                    .get_config(tool.name())
                    .and_then(|c| c.description.clone())
                    .unwrap_or_else(|| tool.description().to_string());

                ToolDefinition {
                    name: tool.name().to_string(),
                    description,
                    parameters: tool.parameters_schema(),
                }
            })
            .collect()
    }

    /// Get all enabled tool names
    pub fn enabled_tool_names(&self) -> Vec<&str> {
        self.tools
            .keys()
            .filter(|name| self.is_enabled(name))
            .map(|s| s.as_str())
            .collect()
    }

    /// Get a tool by name
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Check if a tool exists
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Execute a tool by name (respects enabled status)
    pub async fn execute(&self, name: &str, args: Value) -> Result<Value> {
        if !self.is_enabled(name) {
            return Err(crate::types::AppError::InvalidInput(format!(
                "Tool '{}' is disabled",
                name
            )));
        }

        if let Some(tool) = self.tools.get(name) {
            tool.execute(args).await
        } else {
            Err(crate::types::AppError::NotFound(format!(
                "Tool not found: {}",
                name
            )))
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_enabled_default() {
        let registry = ToolRegistry::new();
        // Unknown tools default to enabled
        assert!(registry.is_enabled("unknown"));
    }

    #[test]
    fn test_tool_disabled() {
        let mut registry = ToolRegistry::new();
        registry.set_config(
            "test",
            ToolConfig {
                enabled: false,
                description: None,
                timeout_secs: 30,
                extra: HashMap::new(),
            },
        );
        assert!(!registry.is_enabled("test"));
    }

    #[test]
    fn test_tool_timeout() {
        let mut registry = ToolRegistry::new();
        registry.set_config(
            "test",
            ToolConfig {
                enabled: true,
                description: None,
                timeout_secs: 60,
                extra: HashMap::new(),
            },
        );
        assert_eq!(registry.get_timeout("test"), 60);
        assert_eq!(registry.get_timeout("unknown"), 30); // Default
    }
}
