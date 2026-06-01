use ares_types::types::{Result, ToolDefinition};
use ares_config::toml_config::{AresConfig, ToolConfig};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

/// Trait for implementing tools that agents can invoke.
///
/// Tools provide specific capabilities to agents, such as calculations,
/// web searches, or API calls.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the unique name of this tool.
    fn name(&self) -> &str;
    /// Returns a description of what this tool does.
    fn description(&self) -> &str;
    /// Returns the JSON schema for this tool's parameters.
    fn parameters_schema(&self) -> Value;
    /// Executes the tool with the given arguments.
    async fn execute(&self, args: Value) -> Result<Value>;
}

/// Registry for managing tools with configuration support
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    configs: HashMap<String, ToolConfig>,
}

impl ToolRegistry {
    /// Creates an empty tool registry.
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
        self.configs.get(name).map(|c| c.enabled).unwrap_or(true) // Default to enabled if no config
    }

    /// Get timeout for a tool
    pub fn get_timeout(&self, name: &str) -> u64 {
        self.configs.get(name).map(|c| c.timeout_secs).unwrap_or(30) // Default 30 seconds
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
            return Err(ares_types::AppError::InvalidInput(format!(
                "Tool '{}' is disabled",
                name
            )));
        }

        let Some(tool) = self.tools.get(name) else {
            return Err(ares_types::AppError::NotFound(format!(
                "Tool not found: {}",
                name
            )));
        };

        let timeout_secs = self.get_timeout(name);
        match timeout(Duration::from_secs(timeout_secs), tool.execute(args)).await {
            Ok(result) => result,
            Err(_) => Err(ares_types::AppError::Unavailable(format!(
                "Tool '{}' execution timed out after {}s",
                name, timeout_secs
            ))),
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
    use ares_config::toml_config::{
        AresConfig, AuthConfig, BillingConfig, DatabaseConfig, DynamicConfigPaths, RagConfig,
        ServerConfig, ToolConfig,
    };
    use ares_types::AppError;
    use serde_json::json;

    struct MockTool {
        tool_name: &'static str,
        tool_description: &'static str,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            self.tool_name
        }

        fn description(&self) -> &str {
            self.tool_description
        }

        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(&self, args: Value) -> Result<Value> {
            Ok(json!({ "tool": self.tool_name, "args": args }))
        }
    }


    struct SlowTool {
        delay: Duration,
    }

    #[async_trait]
    impl Tool for SlowTool {
        fn name(&self) -> &str {
            "slow"
        }

        fn description(&self) -> &str {
            "Sleeps before returning"
        }

        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }

        async fn execute(&self, _args: Value) -> Result<Value> {
            tokio::time::sleep(self.delay).await;
            Ok(json!({ "done": true }))
        }
    }

    struct FailingTool;

    #[async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            "failing"
        }

        fn description(&self) -> &str {
            "Always fails"
        }

        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }

        async fn execute(&self, _args: Value) -> Result<Value> {
            Err(AppError::InvalidInput("tool execution failed".into()))
        }
    }

    fn mock_tool(name: &'static str, description: &'static str) -> Arc<dyn Tool> {
        Arc::new(MockTool {
            tool_name: name,
            tool_description: description,
        })
    }

    fn disabled_config() -> ToolConfig {
        ToolConfig {
            enabled: false,
            description: None,
            timeout_secs: 30,
            extra: HashMap::new(),
        }
    }

    fn minimal_ares_config(tools: HashMap<String, ToolConfig>) -> AresConfig {
        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            database: DatabaseConfig::default(),
            providers: HashMap::new(),
            models: HashMap::new(),
            tools,
            agents: HashMap::new(),
            workflows: HashMap::new(),
            rag: RagConfig::default(),
            billing: BillingConfig {
                model_pricing: HashMap::new(),
            },
            config: DynamicConfigPaths::default(),
        }
    }

    #[test]
    fn test_registry_default_is_empty() {
        let registry = ToolRegistry::default();
        assert!(!registry.has_tool("anything"));
        assert!(registry.get("anything").is_none());
        assert!(registry.get_tool_definitions().is_empty());
        assert!(registry.enabled_tool_names().is_empty());
    }

    #[test]
    fn test_register_and_lookup() {
        let mut registry = ToolRegistry::new();
        registry.register(mock_tool("alpha", "Alpha tool"));

        assert!(registry.has_tool("alpha"));
        assert!(!registry.has_tool("missing"));
        assert!(registry.get("alpha").is_some());
        assert_eq!(registry.get("alpha").unwrap().name(), "alpha");
        assert!(registry.get("missing").is_none());
    }

    #[test]
    fn test_register_overwrites_existing_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(mock_tool("dup", "first"));
        registry.register(mock_tool("dup", "second"));

        assert_eq!(registry.get("dup").unwrap().description(), "second");
        assert_eq!(registry.get_tool_definitions().len(), 1);
    }

    #[test]
    fn test_register_with_config() {
        let mut registry = ToolRegistry::new();
        let config = ToolConfig {
            enabled: false,
            description: Some("configured".into()),
            timeout_secs: 45,
            extra: HashMap::new(),
        };
        registry.register_with_config(mock_tool("beta", "Beta tool"), config);

        assert!(registry.has_tool("beta"));
        assert!(!registry.is_enabled("beta"));
        assert_eq!(registry.get_timeout("beta"), 45);
        let stored = registry.get_config("beta").unwrap();
        assert_eq!(stored.description.as_deref(), Some("configured"));
    }

    #[test]
    fn test_with_config_loads_tool_configs() {
        let mut tools = HashMap::new();
        tools.insert(
            "from_toml".to_string(),
            ToolConfig {
                enabled: false,
                description: Some("from config".into()),
                timeout_secs: 99,
                extra: HashMap::new(),
            },
        );
        let config = minimal_ares_config(tools);
        let registry = ToolRegistry::with_config(&config);

        assert!(!registry.is_enabled("from_toml"));
        assert_eq!(registry.get_timeout("from_toml"), 99);
        assert_eq!(
            registry
                .get_config("from_toml")
                .unwrap()
                .description
                .as_deref(),
            Some("from config")
        );
        assert!(!registry.has_tool("from_toml"));
    }

    #[test]
    fn test_enabled_tool_names_and_definitions_iteration() {
        let mut registry = ToolRegistry::new();
        registry.register(mock_tool("alpha", "Alpha"));
        registry.register(mock_tool("beta", "Beta"));
        registry.register(mock_tool("gamma", "Gamma"));
        registry.set_config("beta", disabled_config());

        let mut names: Vec<_> = registry.enabled_tool_names();
        names.sort_unstable();
        assert_eq!(names, vec!["alpha", "gamma"]);

        let mut definitions = registry.get_tool_definitions();
        definitions.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(
            definitions
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "gamma"]
        );
        assert_eq!(definitions[0].description, "Alpha");
    }

    #[test]
    fn test_get_tool_definitions_for_filters_names_and_enabled() {
        let mut registry = ToolRegistry::new();
        registry.register(mock_tool("alpha", "Alpha"));
        registry.register(mock_tool("beta", "Beta"));
        registry.register(mock_tool("gamma", "Gamma"));
        registry.set_config("beta", disabled_config());

        let mut definitions =
            registry.get_tool_definitions_for(&["beta", "gamma", "missing"]);
        definitions.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name, "gamma");
        assert_eq!(definitions[0].description, "Gamma");
    }

    #[test]
    fn test_config_description_overrides_tool_description() {
        let mut registry = ToolRegistry::new();
        registry.register(mock_tool("alpha", "Built-in description"));
        registry.set_config(
            "alpha",
            ToolConfig {
                enabled: true,
                description: Some("Configured description".into()),
                timeout_secs: 30,
                extra: HashMap::new(),
            },
        );

        let definitions = registry.get_tool_definitions();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].description, "Configured description");
        assert_eq!(definitions[0].parameters["type"], "object");
    }

    #[test]
    fn test_tool_enabled_default() {
        let registry = ToolRegistry::new();
        // Unknown tools default to enabled
        assert!(registry.is_enabled("unknown"));
    }

    #[test]
    fn test_tool_disabled() {
        let mut registry = ToolRegistry::new();
        registry.set_config("test", disabled_config());
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

    #[tokio::test]
    async fn test_execute_success() {
        let mut registry = ToolRegistry::new();
        registry.register(mock_tool("echo", "Echo"));
        let args = json!({ "value": "hello" });

        let result = registry.execute("echo", args.clone()).await.unwrap();
        assert_eq!(result["tool"], "echo");
        assert_eq!(result["args"], args);
    }

    #[tokio::test]
    async fn test_execute_not_found() {
        let registry = ToolRegistry::new();
        let err = registry
            .execute("missing", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(msg) if msg.contains("missing")));
    }

    #[tokio::test]
    async fn test_execute_disabled_tool() {
        let mut registry = ToolRegistry::new();
        registry.register(mock_tool("blocked", "Blocked"));
        registry.set_config("blocked", disabled_config());

        let err = registry
            .execute("blocked", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AppError::InvalidInput(msg) if msg.contains("disabled")
        ));
    }

    #[tokio::test]
    async fn test_execute_propagates_tool_error() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FailingTool));

        let err = registry
            .execute("failing", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AppError::InvalidInput(msg) if msg.contains("execution failed")
        ));
    }
    #[test]
    fn test_tool_config_serde_roundtrip() {
        let tool = ToolConfig {
            enabled: false,
            description: Some("from toml".into()),
            timeout_secs: 42,
            extra: HashMap::new(),
        };
        let decoded: ToolConfig = toml::from_str(&toml::to_string(&tool).unwrap()).unwrap();
        assert!(!decoded.enabled);
        assert_eq!(decoded.description.as_deref(), Some("from toml"));
        assert_eq!(decoded.timeout_secs, 42);
    }

    #[test]
    fn test_with_config_loads_tools_from_toml() {
        let content = r#"
[server]
[auth]
jwt_secret_env = "TEST_JWT"
api_key_env = "TEST_API"
[database]
[tools.calculator]
enabled = false
timeout_secs = 12
description = "Calc tool"
"#;
        let config: AresConfig = toml::from_str(content).unwrap();
        let registry = ToolRegistry::with_config(&config);

        assert!(!registry.is_enabled("calculator"));
        assert_eq!(registry.get_timeout("calculator"), 12);
        assert_eq!(
            registry
                .get_config("calculator")
                .unwrap()
                .description
                .as_deref(),
            Some("Calc tool")
        );
    }

    #[tokio::test]
    async fn test_execute_completes_within_timeout() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(SlowTool {
            delay: Duration::from_millis(50),
        }));
        registry.set_config(
            "slow",
            ToolConfig {
                enabled: true,
                description: None,
                timeout_secs: 2,
                extra: HashMap::new(),
            },
        );

        let result = registry.execute("slow", json!({})).await.unwrap();
        assert_eq!(result["done"], true);
    }

    #[tokio::test]
    async fn test_execute_timeout_path() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(SlowTool {
            delay: Duration::from_secs(2),
        }));
        registry.set_config(
            "slow",
            ToolConfig {
                enabled: true,
                description: None,
                timeout_secs: 1,
                extra: HashMap::new(),
            },
        );

        let err = registry.execute("slow", json!({})).await.unwrap_err();
        assert!(matches!(
            err,
            AppError::Unavailable(msg) if msg.contains("timed out") && msg.contains("slow")
        ));
    }

}
