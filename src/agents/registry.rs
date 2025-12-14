//! Agent Registry for managing configurable agents
//!
//! This module provides a registry for creating and managing agents
//! based on TOML configuration.

use crate::agents::configurable::ConfigurableAgent;
use crate::llm::ProviderRegistry;
use crate::tools::registry::ToolRegistry;
use crate::types::{AgentType, AppError, Result};
use crate::utils::toml_config::{AgentConfig, AresConfig};
use std::collections::HashMap;
use std::sync::Arc;

/// Registry for managing agent configurations and creating agent instances
pub struct AgentRegistry {
    /// Agent configurations keyed by name
    configs: HashMap<String, AgentConfig>,
    /// Provider registry for creating LLM clients
    provider_registry: Arc<ProviderRegistry>,
    /// Tool registry shared across agents
    tool_registry: Arc<ToolRegistry>,
}

impl AgentRegistry {
    /// Create a new agent registry
    pub fn new(provider_registry: Arc<ProviderRegistry>, tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            configs: HashMap::new(),
            provider_registry,
            tool_registry,
        }
    }

    /// Create an agent registry from TOML configuration
    pub fn from_config(
        config: &AresConfig,
        provider_registry: Arc<ProviderRegistry>,
        tool_registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            configs: config.agents.clone(),
            provider_registry,
            tool_registry,
        }
    }

    /// Register an agent configuration
    pub fn register(&mut self, name: &str, config: AgentConfig) {
        self.configs.insert(name.to_string(), config);
    }

    /// Get an agent configuration by name
    pub fn get_config(&self, name: &str) -> Option<&AgentConfig> {
        self.configs.get(name)
    }

    /// Get all agent names
    pub fn agent_names(&self) -> Vec<&str> {
        self.configs.keys().map(|s| s.as_str()).collect()
    }

    /// Check if an agent exists
    pub fn has_agent(&self, name: &str) -> bool {
        self.configs.contains_key(name)
    }

    /// Create an agent instance by name
    ///
    /// This creates a new ConfigurableAgent with the appropriate LLM client
    /// and tool registry based on the agent's configuration.
    pub async fn create_agent(&self, name: &str) -> Result<ConfigurableAgent> {
        let config = self.get_config(name).ok_or_else(|| {
            AppError::Configuration(format!("Agent '{}' not found in configuration", name))
        })?;

        // Create the LLM client for this agent's model
        let llm = self
            .provider_registry
            .create_client_for_model(&config.model)
            .await?;

        // Create a filtered tool registry with only the tools this agent can use
        let agent_tool_registry = if config.tools.is_empty() {
            None
        } else {
            Some(Arc::clone(&self.tool_registry))
        };

        Ok(ConfigurableAgent::new(
            name,
            config,
            llm,
            agent_tool_registry,
        ))
    }

    /// Create an agent instance for a specific AgentType
    pub async fn create_agent_by_type(&self, agent_type: AgentType) -> Result<ConfigurableAgent> {
        let name = Self::type_to_name(agent_type);
        self.create_agent(name).await
    }

    /// Convert AgentType to agent name
    pub fn type_to_name(agent_type: AgentType) -> &'static str {
        match agent_type {
            AgentType::Router => "router",
            AgentType::Orchestrator => "orchestrator",
            AgentType::Product => "product",
            AgentType::Invoice => "invoice",
            AgentType::Sales => "sales",
            AgentType::Finance => "finance",
            AgentType::HR => "hr",
        }
    }

    /// Get the model name for an agent
    pub fn get_agent_model(&self, name: &str) -> Option<&str> {
        self.configs.get(name).map(|c| c.model.as_str())
    }

    /// Get the tools for an agent
    pub fn get_agent_tools(&self, name: &str) -> Vec<&str> {
        self.configs
            .get(name)
            .map(|c| c.tools.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// Get the system prompt for an agent (if custom)
    pub fn get_agent_system_prompt(&self, name: &str) -> Option<&str> {
        self.configs
            .get(name)
            .and_then(|c| c.system_prompt.as_deref())
    }
}

/// Builder for creating AgentRegistry with fluent API
pub struct AgentRegistryBuilder {
    configs: HashMap<String, AgentConfig>,
    provider_registry: Option<Arc<ProviderRegistry>>,
    tool_registry: Option<Arc<ToolRegistry>>,
}

impl AgentRegistryBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            provider_registry: None,
            tool_registry: None,
        }
    }

    /// Set the provider registry
    pub fn with_provider_registry(mut self, registry: Arc<ProviderRegistry>) -> Self {
        self.provider_registry = Some(registry);
        self
    }

    /// Set the tool registry
    pub fn with_tool_registry(mut self, registry: Arc<ToolRegistry>) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    /// Add an agent configuration
    pub fn with_agent(mut self, name: &str, config: AgentConfig) -> Self {
        self.configs.insert(name.to_string(), config);
        self
    }

    /// Load agent configurations from TOML config
    pub fn from_config(mut self, config: &AresConfig) -> Self {
        self.configs = config.agents.clone();
        self
    }

    /// Build the AgentRegistry
    pub fn build(self) -> Result<AgentRegistry> {
        let provider_registry = self.provider_registry.ok_or_else(|| {
            AppError::Configuration("ProviderRegistry is required for AgentRegistry".into())
        })?;

        let tool_registry = self
            .tool_registry
            .unwrap_or_else(|| Arc::new(ToolRegistry::new()));

        Ok(AgentRegistry {
            configs: self.configs,
            provider_registry,
            tool_registry,
        })
    }
}

impl Default for AgentRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::toml_config::ProviderConfig;
    use std::collections::HashMap;

    fn create_test_provider_registry() -> Arc<ProviderRegistry> {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "ollama-local",
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".to_string(),
                default_model: "granite4:tiny-h".to_string(),
            },
        );
        registry.register_model(
            "default",
            crate::utils::toml_config::ModelConfig {
                provider: "ollama-local".to_string(),
                model: "granite4:tiny-h".to_string(),
                temperature: 0.7,
                max_tokens: 512,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
            },
        );
        Arc::new(registry)
    }

    #[test]
    fn test_type_to_name() {
        assert_eq!(AgentRegistry::type_to_name(AgentType::Router), "router");
        assert_eq!(AgentRegistry::type_to_name(AgentType::Product), "product");
        assert_eq!(AgentRegistry::type_to_name(AgentType::HR), "hr");
        assert_eq!(AgentRegistry::type_to_name(AgentType::Invoice), "invoice");
        assert_eq!(AgentRegistry::type_to_name(AgentType::Sales), "sales");
        assert_eq!(AgentRegistry::type_to_name(AgentType::Finance), "finance");
        assert_eq!(
            AgentRegistry::type_to_name(AgentType::Orchestrator),
            "orchestrator"
        );
    }

    #[test]
    fn test_registry_register_and_get() {
        let provider_registry = create_test_provider_registry();
        let tool_registry = Arc::new(ToolRegistry::new());
        let mut registry = AgentRegistry::new(provider_registry, tool_registry);

        let config = AgentConfig {
            model: "default".to_string(),
            system_prompt: Some("Test prompt".to_string()),
            tools: vec![],
            max_tool_iterations: 5,
            parallel_tools: false,
            extra: HashMap::new(),
        };

        registry.register("test-agent", config);

        assert!(registry.has_agent("test-agent"));
        assert!(!registry.has_agent("nonexistent"));
        assert!(registry.get_config("test-agent").is_some());
        assert!(registry.get_config("nonexistent").is_none());
    }

    #[test]
    fn test_registry_agent_names() {
        let provider_registry = create_test_provider_registry();
        let tool_registry = Arc::new(ToolRegistry::new());
        let mut registry = AgentRegistry::new(provider_registry, tool_registry);

        registry.register(
            "agent1",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );

        registry.register(
            "agent2",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );

        let names = registry.agent_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"agent1"));
        assert!(names.contains(&"agent2"));
    }

    #[test]
    fn test_registry_get_agent_model() {
        let provider_registry = create_test_provider_registry();
        let tool_registry = Arc::new(ToolRegistry::new());
        let mut registry = AgentRegistry::new(provider_registry, tool_registry);

        registry.register(
            "test",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );

        assert_eq!(registry.get_agent_model("test"), Some("default"));
        assert_eq!(registry.get_agent_model("nonexistent"), None);
    }

    #[test]
    fn test_registry_get_agent_tools() {
        let provider_registry = create_test_provider_registry();
        let tool_registry = Arc::new(ToolRegistry::new());
        let mut registry = AgentRegistry::new(provider_registry, tool_registry);

        registry.register(
            "with_tools",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: None,
                tools: vec!["calculator".to_string(), "web_search".to_string()],
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );

        registry.register(
            "no_tools",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );

        let tools = registry.get_agent_tools("with_tools");
        assert_eq!(tools.len(), 2);
        assert!(tools.contains(&"calculator"));

        let no_tools = registry.get_agent_tools("no_tools");
        assert!(no_tools.is_empty());
    }

    #[test]
    fn test_builder_build_without_provider_registry() {
        let result = AgentRegistryBuilder::new()
            .with_tool_registry(Arc::new(ToolRegistry::new()))
            .build();

        assert!(result.is_err());
    }

    #[test]
    fn test_builder_build_success() {
        let provider_registry = create_test_provider_registry();

        let result = AgentRegistryBuilder::new()
            .with_provider_registry(provider_registry)
            .with_agent(
                "test",
                AgentConfig {
                    model: "default".to_string(),
                    system_prompt: Some("Test".to_string()),
                    tools: vec![],
                    max_tool_iterations: 5,
                    parallel_tools: false,
                    extra: HashMap::new(),
                },
            )
            .build();

        assert!(result.is_ok());
        let registry = result.unwrap();
        assert!(registry.has_agent("test"));
    }
}
