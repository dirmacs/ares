//! Agent Registry for managing configurable agents
//!
//! This module provides a registry for creating and managing agents
//! based on both TOML and TOON configuration.
//!
//! ## Configuration Precedence
//!
//! When looking up an agent by name:
//! 1. TOML config (`ares.toml` [agents.*]) is checked first
//! 2. TOON config (`config/agents/*.toon`) is checked second
//!
//! This allows TOML to override TOON configs for specific deployments.

use crate::agents::configurable::ConfigurableAgent;
use crate::llm::ProviderRegistry;
use crate::tools::registry::ToolRegistry;
use crate::types::{AgentType, AppError, Result};
use crate::utils::toml_config::{AgentConfig, AresConfig};
use crate::utils::toon_config::{DynamicConfigManager, ToonAgentConfig};
use std::collections::HashMap;
use std::sync::Arc;

/// Registry for managing agent configurations and creating agent instances
///
/// Supports both TOML-based static config and TOON-based dynamic config.
/// TOML configs take precedence over TOON configs when both exist.
pub struct AgentRegistry {
    /// Agent configurations from TOML keyed by name
    configs: HashMap<String, AgentConfig>,
    /// Provider registry for creating LLM clients
    provider_registry: Arc<ProviderRegistry>,
    /// Tool registry shared across agents
    tool_registry: Arc<ToolRegistry>,
    /// Optional TOON-based dynamic config manager for hot-reloadable agents
    dynamic_config: Option<Arc<DynamicConfigManager>>,
}

impl AgentRegistry {
    /// Create a new agent registry
    pub fn new(provider_registry: Arc<ProviderRegistry>, tool_registry: Arc<ToolRegistry>) -> Self {
        Self {
            configs: HashMap::new(),
            provider_registry,
            tool_registry,
            dynamic_config: None,
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
            dynamic_config: None,
        }
    }

    /// Create an agent registry with both TOML and TOON config support
    pub fn with_dynamic_config(
        config: &AresConfig,
        provider_registry: Arc<ProviderRegistry>,
        tool_registry: Arc<ToolRegistry>,
        dynamic_config: Arc<DynamicConfigManager>,
    ) -> Self {
        Self {
            configs: config.agents.clone(),
            provider_registry,
            tool_registry,
            dynamic_config: Some(dynamic_config),
        }
    }

    /// Set the dynamic config manager for TOON support
    pub fn set_dynamic_config(&mut self, dynamic_config: Arc<DynamicConfigManager>) {
        self.dynamic_config = Some(dynamic_config);
    }

    /// Register an agent configuration
    pub fn register(&mut self, name: &str, config: AgentConfig) {
        self.configs.insert(name.to_string(), config);
    }

    /// Get an agent configuration by name (TOML only)
    ///
    /// Note: For lookups that include TOON, use `get_config_any` instead.
    pub fn get_config(&self, name: &str) -> Option<&AgentConfig> {
        self.configs.get(name)
    }

    /// Get TOON agent config by name
    pub fn get_toon_config(&self, name: &str) -> Option<ToonAgentConfig> {
        self.dynamic_config.as_ref().and_then(|dc| dc.agent(name))
    }

    /// Check if an agent exists in TOML config
    fn has_toml_agent(&self, name: &str) -> bool {
        self.configs.contains_key(name)
    }

    /// Check if an agent exists in TOON config
    fn has_toon_agent(&self, name: &str) -> bool {
        self.dynamic_config
            .as_ref()
            .map(|dc| dc.agent(name).is_some())
            .unwrap_or(false)
    }

    /// Get all agent names (from both TOML and TOON)
    pub fn agent_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.configs.keys().cloned().collect();

        // Add TOON agent names that aren't already in TOML
        if let Some(dc) = &self.dynamic_config {
            for name in dc.agent_names() {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }

        names
    }

    /// Check if an agent exists (in either TOML or TOON config)
    pub fn has_agent(&self, name: &str) -> bool {
        self.has_toml_agent(name) || self.has_toon_agent(name)
    }

    /// Convert ToonAgentConfig to AgentConfig for unified handling
    fn toon_to_agent_config(toon: &ToonAgentConfig) -> AgentConfig {
        AgentConfig {
            model: toon.model.clone(),
            system_prompt: toon.system_prompt.clone(),
            tools: toon.tools.clone(),
            max_tool_iterations: toon.max_tool_iterations,
            parallel_tools: toon.parallel_tools,
            // Convert serde_json::Value to toml::Value
            // For extra fields we just convert to string representation
            extra: toon
                .extra
                .iter()
                .filter_map(|(k, v)| {
                    // Convert JSON value to TOML value
                    match v {
                        serde_json::Value::String(s) => {
                            Some((k.clone(), toml::Value::String(s.clone())))
                        }
                        serde_json::Value::Number(n) => n
                            .as_i64()
                            .map(|i| (k.clone(), toml::Value::Integer(i)))
                            .or_else(|| n.as_f64().map(|f| (k.clone(), toml::Value::Float(f)))),
                        serde_json::Value::Bool(b) => Some((k.clone(), toml::Value::Boolean(*b))),
                        _ => {
                            // For arrays/objects, convert to string
                            Some((k.clone(), toml::Value::String(v.to_string())))
                        }
                    }
                })
                .collect(),
        }
    }

    /// Create an agent instance by name
    ///
    /// This creates a new ConfigurableAgent with the appropriate LLM client
    /// and tool registry based on the agent's configuration.
    ///
    /// Lookup order:
    /// 1. TOML config (`ares.toml` [agents.*])
    /// 2. TOON config (`config/agents/*.toon`)
    pub async fn create_agent(&self, name: &str) -> Result<ConfigurableAgent> {
        // First check TOML config
        if let Some(config) = self.get_config(name) {
            return self.create_agent_from_config(name, config).await;
        }

        // Then check TOON config
        if let Some(toon_config) = self.get_toon_config(name) {
            let config = Self::toon_to_agent_config(&toon_config);
            return self.create_agent_from_config(name, &config).await;
        }

        Err(AppError::Configuration(format!(
            "Agent '{}' not found in TOML or TOON configuration",
            name
        )))
    }

    /// Create an agent instance from an explicit configuration
    pub async fn create_agent_from_config(
        &self,
        name: &str,
        config: &AgentConfig,
    ) -> Result<ConfigurableAgent> {
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
        let name = Self::type_to_name(&agent_type);
        self.create_agent(name).await
    }

    /// Convert AgentType to agent name
    pub fn type_to_name(agent_type: &AgentType) -> &str {
        agent_type.as_str()
    }

    /// Get the model name for an agent (checks both TOML and TOON)
    pub fn get_agent_model(&self, name: &str) -> Option<String> {
        // Check TOML first
        if let Some(config) = self.configs.get(name) {
            return Some(config.model.clone());
        }
        // Check TOON
        self.get_toon_config(name).map(|c| c.model)
    }

    /// Get the tools for an agent (checks both TOML and TOON)
    pub fn get_agent_tools(&self, name: &str) -> Vec<String> {
        // Check TOML first
        if let Some(config) = self.configs.get(name) {
            return config.tools.clone();
        }
        // Check TOON
        self.get_toon_config(name)
            .map(|c| c.tools)
            .unwrap_or_default()
    }

    /// Get the system prompt for an agent (checks both TOML and TOON)
    pub fn get_agent_system_prompt(&self, name: &str) -> Option<String> {
        // Check TOML first
        if let Some(config) = self.configs.get(name) {
            return config.system_prompt.clone();
        }
        // Check TOON
        self.get_toon_config(name).and_then(|c| c.system_prompt)
    }
}

/// Builder for creating AgentRegistry with fluent API
pub struct AgentRegistryBuilder {
    configs: HashMap<String, AgentConfig>,
    provider_registry: Option<Arc<ProviderRegistry>>,
    tool_registry: Option<Arc<ToolRegistry>>,
    dynamic_config: Option<Arc<DynamicConfigManager>>,
}

impl AgentRegistryBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            provider_registry: None,
            tool_registry: None,
            dynamic_config: None,
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

    /// Set the dynamic config manager for TOON support
    pub fn with_dynamic_config(mut self, dynamic_config: Arc<DynamicConfigManager>) -> Self {
        self.dynamic_config = Some(dynamic_config);
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
            dynamic_config: self.dynamic_config,
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
                default_model: "ministral-3:3b".to_string(),
            },
        );
        registry.register_model(
            "default",
            crate::utils::toml_config::ModelConfig {
                provider: "ollama-local".to_string(),
                model: "ministral-3:3b".to_string(),
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
        assert_eq!(AgentRegistry::type_to_name(&AgentType::Router), "router");
        assert_eq!(AgentRegistry::type_to_name(&AgentType::Product), "product");
        assert_eq!(AgentRegistry::type_to_name(&AgentType::HR), "hr");
        assert_eq!(AgentRegistry::type_to_name(&AgentType::Invoice), "invoice");
        assert_eq!(AgentRegistry::type_to_name(&AgentType::Sales), "sales");
        assert_eq!(AgentRegistry::type_to_name(&AgentType::Finance), "finance");
        assert_eq!(
            AgentRegistry::type_to_name(&AgentType::Orchestrator),
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
        assert!(names.contains(&"agent1".to_string()));
        assert!(names.contains(&"agent2".to_string()));
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

        assert_eq!(
            registry.get_agent_model("test"),
            Some("default".to_string())
        );
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
        assert!(tools.contains(&"calculator".to_string()));

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
