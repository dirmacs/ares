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

use crate::configurable::ConfigurableAgent;
use crate::{AgentConfig, ToonAgents};
use ares_llm::ProviderRegistry;
use ares_tools::Tools;
use ares_types::types::{AgentType, AppError, Result};
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
    /// Tools capability shared across agents
    tools: Arc<Tools>,
    /// Optional TOON-based dynamic agent lookup (Overlay keeps this live)
    dynamic_config: Option<Arc<dyn ToonAgents>>,
}

fn intersect_agent_tools_with_tenant_allowlist(
    agent_tools: Option<&[String]>,
    tenant_allowed_tools: &[String],
) -> Vec<String> {
    agent_tools
        .unwrap_or(&[])
        .iter()
        .filter(|tool| tenant_allowed_tools.contains(*tool))
        .cloned()
        .collect()
}

impl AgentRegistry {
    /// Create a new agent registry
    pub fn new(provider_registry: Arc<ProviderRegistry>, tools: Arc<Tools>) -> Self {
        Self {
            configs: HashMap::new(),
            provider_registry,
            tools,
            dynamic_config: None,
        }
    }

    /// Create an agent registry from TOML agent configs
    pub fn from_config(
        agents: HashMap<String, AgentConfig>,
        provider_registry: Arc<ProviderRegistry>,
        tools: Arc<Tools>,
    ) -> Self {
        Self {
            configs: agents,
            provider_registry,
            tools,
            dynamic_config: None,
        }
    }

    /// Create an agent registry with both TOML and TOON config support
    pub fn with_dynamic_config(
        agents: HashMap<String, AgentConfig>,
        provider_registry: Arc<ProviderRegistry>,
        tools: Arc<Tools>,
        dynamic_config: Arc<dyn ToonAgents>,
    ) -> Self {
        Self {
            configs: agents,
            provider_registry,
            tools,
            dynamic_config: Some(dynamic_config),
        }
    }

    /// Set the dynamic TOON agent lookup
    pub fn set_dynamic_config(&mut self, dynamic_config: Arc<dyn ToonAgents>) {
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

    /// Get an agent configuration by name from TOML or TOON.
    pub fn get_config_any(&self, name: &str) -> Option<AgentConfig> {
        self.configs.get(name).cloned().or_else(|| self.get_toon_config(name))
    }

    /// Get TOON agent config by name (already converted to AgentConfig)
    pub fn get_toon_config(&self, name: &str) -> Option<AgentConfig> {
        self.dynamic_config.as_ref().and_then(|dc| dc.get(name))
    }

    /// Check if an agent exists in TOML config
    fn has_toml_agent(&self, name: &str) -> bool {
        self.configs.contains_key(name)
    }

    /// Check if an agent exists in TOON config
    fn has_toon_agent(&self, name: &str) -> bool {
        self.dynamic_config
            .as_ref()
            .map(|dc| dc.get(name).is_some())
            .unwrap_or(false)
    }

    /// Get all agent names (from both TOML and TOON)
    pub fn agent_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.configs.keys().cloned().collect();

        // Add TOON agent names that aren't already in TOML
        if let Some(dc) = &self.dynamic_config {
            for name in dc.names() {
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
        if let Some(config) = self.get_toon_config(name) {
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
        let provider_name = self
            .provider_registry
            .get_model(&config.model)
            .map(|model| model.provider.clone())
            .unwrap_or_else(|| config.model.clone());

        let mut agent = ConfigurableAgent::new_with_provider(
            name,
            config,
            llm,
            None,
            provider_name,
        );
        agent.set_tools(Arc::clone(&self.tools));
        Ok(agent)
    }

    /// Create an agent instance from an explicit configuration with tier
    /// resolution and fallback providers wired in.
    #[cfg(feature = "postgres")]
    pub async fn create_agent_from_config_with_fallbacks(
        &self,
        name: &str,
        config: &AgentConfig,
        tenant_id: &str,
        pool: &sqlx::PgPool,
        fleet_secrets: &ares_store::FleetSecrets,
    ) -> Result<ConfigurableAgent> {
        let chain = self
            .provider_registry
            .resolve_with_fallback(&config.model, tenant_id, pool, fleet_secrets)
            .await?;

        let allowlist_store = ares_store::tenant_allowlist::TenantAllowlistStore::new(pool);
        for resolved in &chain {
            if !allowlist_store
                .is_model_allowed(tenant_id, &resolved.model_name)
                .await
                .map_err(|e| AppError::Auth(format!("Failed to check model allowlist: {}", e)))?
            {
                return Err(AppError::Auth(format!(
                    "Model '{}' is not allowed for this tenant",
                    resolved.model_name
                )));
            }
        }

        let mut iter = chain.into_iter();
        let primary = iter.next().ok_or_else(|| {
            AppError::Configuration(format!(
                "No provider resolved for model/tier '{}'",
                config.model
            ))
        })?;

        let primary_provider_name = primary.provider_name.clone();

        let llm = self
            .provider_registry
            .create_client_for_resolved_provider(&primary)
            .await
            .map_err(|e| {
                AppError::Configuration(format!(
                    "Failed to construct primary provider '{}' for model '{}': {}",
                    primary.provider_name, primary.model_name, e
                ))
            })?;

        let mut fallback_llms = Vec::new();
        for fallback in iter {
            let fallback_provider_name = fallback.provider_name.clone();
            let client = self
                .provider_registry
                .create_client_for_resolved_provider(&fallback)
                .await
                .map_err(|e| {
                    AppError::Configuration(format!(
                        "Failed to construct fallback provider '{}' for model '{}': {}",
                        fallback.provider_name, fallback.model_name, e
                    ))
                })?;
            fallback_llms.push((fallback_provider_name, client));
        }

        let mut agent = ConfigurableAgent::new_with_provider(
            name,
            config,
            llm,
            None,
            primary_provider_name,
        );
        agent.set_tools(Arc::clone(&self.tools));
        agent.set_fallback_llms_with_providers(fallback_llms);

        // --- tenant allowlist enforcement ---
        // Tool enforcement: intersect config allowed_tools with tenant allowlist.
        // Empty tenant allowlist rows mean default-deny.
        let db_tools = allowlist_store
            .list_tools(tenant_id)
            .await
            .map_err(|e| AppError::Auth(format!("Failed to check tool allowlist: {}", e)))?;
        let db_tool_names: Vec<String> = db_tools.iter().map(|t| t.tool_name.clone()).collect();
        let new_allowed =
            intersect_agent_tools_with_tenant_allowlist(agent.allowed_tools(), &db_tool_names);
        agent.set_allowed_tools(Some(new_allowed));

        Ok(agent)
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

    /// Get the tools for an agent (checks both TOML and TOON).
    /// Returns the explicit allowed_tools list, falling back to the legacy
    /// tools field.  An empty result means no configured tools; runtime
    /// execution remains deny-by-default.
    pub fn get_agent_tools(&self, name: &str) -> Vec<String> {
        // Check TOML first
        if let Some(config) = self.configs.get(name) {
            return config
                .allowed_tools
                .clone()
                .unwrap_or_else(|| config.tools.clone());
        }
        // Check TOON
        self.get_toon_config(name)
            .map(|c| c.allowed_tools.unwrap_or(c.tools))
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
    tools: Option<Arc<Tools>>,
    dynamic_config: Option<Arc<dyn ToonAgents>>,
}

impl AgentRegistryBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            provider_registry: None,
            tools: None,
            dynamic_config: None,
        }
    }

    /// Set the provider registry
    pub fn with_provider_registry(mut self, registry: Arc<ProviderRegistry>) -> Self {
        self.provider_registry = Some(registry);
        self
    }

    /// Set the unified tools capability
    pub fn with_tools(mut self, tools: Arc<Tools>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Set the dynamic config manager for TOON support
    pub fn with_dynamic_config(mut self, dynamic_config: Arc<dyn ToonAgents>) -> Self {
        self.dynamic_config = Some(dynamic_config);
        self
    }

    /// Add an agent configuration
    pub fn with_agent(mut self, name: &str, config: AgentConfig) -> Self {
        self.configs.insert(name.to_string(), config);
        self
    }

    /// Load agent configurations from TOML agent map
    pub fn from_config(mut self, agents: HashMap<String, AgentConfig>) -> Self {
        self.configs = agents;
        self
    }

    /// Build the AgentRegistry
    pub fn build(self) -> Result<AgentRegistry> {
        let provider_registry = self.provider_registry.ok_or_else(|| {
            AppError::Configuration("ProviderRegistry is required for AgentRegistry".into())
        })?;

        let tools = self.tools.unwrap_or_else(|| {
            Arc::new(Tools::from_static(std::iter::empty::<Arc<dyn ares_tools::Tool>>()))
        });

        Ok(AgentRegistry {
            configs: self.configs,
            provider_registry,
            tools,
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
    use ares_llm::ProviderConfig;
    use ares_tools::Tool;
    use std::collections::HashMap;

    struct MapToon(HashMap<String, AgentConfig>);
    impl ToonAgents for MapToon {
        fn get(&self, name: &str) -> Option<AgentConfig> {
            self.0.get(name).cloned()
        }
        fn names(&self) -> Vec<String> {
            self.0.keys().cloned().collect()
        }
    }

    fn empty_tools() -> Arc<Tools> {
        Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new()))
    }

    fn create_test_agent_map() -> HashMap<String, AgentConfig> {
        HashMap::new()
    }

    fn create_test_provider_registry() -> Arc<ProviderRegistry> {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "ollama-local",
            ProviderConfig::OpenAI {
                api_key_env: "TEST_KEY".to_string(),
                api_base: "https://test.example.com/v1".to_string(),
                default_model: "ministral-3:3b".to_string(),
            },
        );
        registry.register_model(
            "default",
            ares_llm::ModelConfig {
                provider: "ollama-local".to_string(),
                model: "ministral-3:3b".to_string(),
                temperature: 0.7,
                max_tokens: 512,
            },
        );
        Arc::new(registry)
    }

    #[test]
    fn intersect_agent_tools_defaults_to_deny() {
        let tenant_tools = vec!["calendar".to_string(), "search".to_string()];
        let allowed = intersect_agent_tools_with_tenant_allowlist(None, &tenant_tools);
        assert!(allowed.is_empty());
    }

    #[test]
    fn intersect_agent_tools_keeps_only_configured_and_tenant_allowed() {
        let agent_tools = vec!["calendar".to_string(), "sql".to_string()];
        let tenant_tools = vec!["calendar".to_string(), "search".to_string()];
        let allowed =
            intersect_agent_tools_with_tenant_allowlist(Some(&agent_tools), &tenant_tools);
        assert_eq!(allowed, vec!["calendar".to_string()]);
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
        let tools = empty_tools();
        let mut registry = AgentRegistry::new(provider_registry, tools);

        let config = AgentConfig {
            model: "default".to_string(),
            system_prompt: Some("Test prompt".to_string()),
            tools: vec![],
            max_tool_iterations: 5,
            parallel_tools: false,
            extra: HashMap::new(),
            allowed_tools: None,
            compaction_enabled: None,
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
        let tools = empty_tools();
        let mut registry = AgentRegistry::new(provider_registry, tools);

        registry.register(
            "agent1",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
                allowed_tools: None,
                compaction_enabled: None,
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
                allowed_tools: None,
                compaction_enabled: None,
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
        let tools = empty_tools();
        let mut registry = AgentRegistry::new(provider_registry, tools);

        registry.register(
            "test",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
                allowed_tools: None,
                compaction_enabled: None,
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
        let tools = empty_tools();
        let mut registry = AgentRegistry::new(provider_registry, tools);

        registry.register(
            "with_tools",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: None,
                tools: vec!["calculator".to_string(), "web_search".to_string()],
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
                allowed_tools: None,
                compaction_enabled: None,
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
                allowed_tools: None,
                compaction_enabled: None,
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
            .with_tools(empty_tools())
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
                    allowed_tools: None,
                    compaction_enabled: None,
                },
            )
            .build();

        assert!(result.is_ok());
        let _registry = result.unwrap();
    }

    // ============================================================
    //  type_to_name for ALL variants (including Custom)
    // ============================================================

    #[test]
    fn test_type_to_name_custom() {
        assert_eq!(
            AgentRegistry::type_to_name(&AgentType::Custom("my-custom-agent".to_string())),
            "my-custom-agent"
        );
    }

    // ============================================================
    //  from_config() - build AgentRegistry from AresConfig
    // ============================================================

    #[test]
    fn test_registry_from_config() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();

        let overlay_config = {
            let mut map = HashMap::new();
            map.insert(
                "toml-agent".to_string(),
                AgentConfig {
                    model: "default".to_string(),
                    system_prompt: Some("TOML prompt".to_string()),
                    tools: vec!["calculator".to_string()],
                    max_tool_iterations: 5,
                    parallel_tools: false,
                    extra: HashMap::new(),
                    allowed_tools: None,
                    compaction_enabled: None,
                },
            );
            map
        };

        let registry = AgentRegistry::from_config(overlay_config, provider_registry, tools);

        assert!(registry.has_agent("toml-agent"));
        assert!(registry.get_config("toml-agent").is_some());
        assert_eq!(
            registry.get_agent_model("toml-agent"),
            Some("default".to_string())
        );
    }

    // ============================================================
    //  AgentRegistryBuilder::from_config(...).with_provider_registry(...).build()
    // ============================================================

    #[test]
    fn test_builder_from_config_with_provider() {
        let provider_registry = create_test_provider_registry();

        let overlay_config = {
            let mut map = HashMap::new();
            map.insert(
                "builder-agent".to_string(),
                AgentConfig {
                    model: "default".to_string(),
                    system_prompt: Some("Builder prompt".to_string()),
                    tools: vec![],
                    max_tool_iterations: 10,
                    parallel_tools: true,
                    extra: HashMap::new(),
                    allowed_tools: None,
                    compaction_enabled: None,
                },
            );
            map
        };

        let result = AgentRegistryBuilder::new()
            .from_config(overlay_config)
            .with_provider_registry(provider_registry)
            .build();

        assert!(result.is_ok());
        let registry = result.unwrap();
        assert!(registry.has_agent("builder-agent"));
    }

    // ============================================================
    //  AgentRegistryBuilder::with_tools(...) and default tools
    // ============================================================

    #[test]
    fn test_builder_with_tools_explicit() {
        let provider_registry = create_test_provider_registry();
        let custom_tools = empty_tools();

        let result = AgentRegistryBuilder::new()
            .with_provider_registry(provider_registry)
            .with_tools(custom_tools.clone())
            .with_agent(
                "tool-agent",
                AgentConfig {
                    model: "default".to_string(),
                    system_prompt: None,
                    tools: vec!["calculator".to_string()],
                    max_tool_iterations: 5,
                    parallel_tools: false,
                    extra: HashMap::new(),
                    allowed_tools: None,
                    compaction_enabled: None,
                },
            )
            .build();

        assert!(result.is_ok());
        let registry = result.unwrap();
        assert!(registry.has_agent("tool-agent"));
    }

    #[test]
    fn test_builder_default_tools() {
        let provider_registry = create_test_provider_registry();

        // Build without calling with_tools - should use default empty Tools
        let result = AgentRegistryBuilder::new()
            .with_provider_registry(provider_registry)
            .with_agent(
                "no-tool-registry-agent",
                AgentConfig {
                    model: "default".to_string(),
                    system_prompt: Some("No explicit tool registry".to_string()),
                    tools: vec![],
                    max_tool_iterations: 5,
                    parallel_tools: false,
                    extra: HashMap::new(),
                    allowed_tools: None,
                    compaction_enabled: None,
                },
            )
            .build();

        assert!(result.is_ok());
        let registry = result.unwrap();
        assert!(registry.has_agent("no-tool-registry-agent"));
    }

    // ============================================================
    //  AgentRegistryBuilder::default() returns equivalent-to-new
    // ============================================================

    #[test]
    fn test_builder_default_is_new() {
        let from_new = AgentRegistryBuilder::new();
        let from_default = AgentRegistryBuilder::default();

        // Neither has provider_registry set, so both should error on build
        assert!(from_new.build().is_err());
        assert!(from_default.build().is_err());
    }

    // ============================================================
    //  get_agent_system_prompt - TOML branch (Some) and None-when-missing
    // ============================================================

    #[test]
    fn test_get_agent_system_prompt_toml_some() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();
        let mut registry = AgentRegistry::new(provider_registry, tools);

        registry.register(
            "has-prompt",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: Some("My System Prompt".to_string()),
                tools: vec![],
                max_tool_iterations: 5,
                parallel_tools: false,
                extra: HashMap::new(),
                allowed_tools: None,
                compaction_enabled: None,
            },
        );

        assert_eq!(
            registry.get_agent_system_prompt("has-prompt"),
            Some("My System Prompt".to_string())
        );
    }

    #[test]
    fn test_get_agent_system_prompt_toml_none() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();
        let mut registry = AgentRegistry::new(provider_registry, tools);

        registry.register(
            "no-prompt",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: None,
                tools: vec![],
                max_tool_iterations: 5,
                parallel_tools: false,
                extra: HashMap::new(),
                allowed_tools: None,
                compaction_enabled: None,
            },
        );

        assert_eq!(registry.get_agent_system_prompt("no-prompt"), None);
        assert_eq!(registry.get_agent_system_prompt("missing"), None);
    }

    // ============================================================
    //  create_agent(name) NOT-found path - Configuration error
    // ============================================================

    #[tokio::test]
    async fn test_create_agent_not_found() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();
        let registry = AgentRegistry::new(provider_registry, tools);

        let result = registry.create_agent("nonexistent-agent").await;

        assert!(result.is_err());
        if let Err(AppError::Configuration(msg)) = result {
            assert!(msg.contains("nonexistent-agent"));
            assert!(msg.contains("not found"));
        } else {
            panic!("Expected Configuration error");
        }
    }

    // ============================================================
    //  create_agent_by_type - error path for unregistered type
    // ============================================================

    #[tokio::test]
    async fn test_create_agent_by_type_not_registered() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();
        let registry = AgentRegistry::new(provider_registry, tools);

        // AgentType::Custom("custom-unregistered".to_string()) returns name "custom-unregistered"
        let result = registry
            .create_agent_by_type(AgentType::Custom("custom-unregistered".to_string()))
            .await;

        assert!(result.is_err());
        if let Err(AppError::Configuration(msg)) = result {
            assert!(msg.contains("custom-unregistered"));
        } else {
            panic!("Expected Configuration error");
        }
    }

    // ============================================================
    //  TOON-backed tests
    // ============================================================

    fn create_test_dynamic_config_manager() -> Arc<dyn ToonAgents> {
        let mut agents = HashMap::new();
        agents.insert(
            "toon-only-agent".to_string(),
            AgentConfig {
                model: "default".to_string(),
                system_prompt: Some("TOON system prompt".to_string()),
                tools: vec!["calculator".to_string()],
                allowed_tools: None,
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
                compaction_enabled: None,
            },
        );
        Arc::new(MapToon(agents))
    }

    #[test]
    fn test_with_dynamic_config() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();
        let dcm = create_test_dynamic_config_manager();

        let registry = AgentRegistry::with_dynamic_config(
            create_test_agent_map(),
            provider_registry,
            tools,
            dcm.clone(),
        );

        assert!(registry.get_toon_config("toon-only-agent").is_some());
        assert!(registry.has_agent("toon-only-agent"));
    }

    #[test]
    fn test_set_dynamic_config() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();
        let dcm = create_test_dynamic_config_manager();

        let mut registry = AgentRegistry::new(provider_registry, tools);
        assert!(!registry.has_agent("toon-only-agent")); // Not set yet

        registry.set_dynamic_config(dcm);
        assert!(registry.has_agent("toon-only-agent"));
        assert!(registry.get_toon_config("toon-only-agent").is_some());
    }

    #[test]
    fn test_toon_merge_agent_names_no_duplicates() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();
        let dcm = create_test_dynamic_config_manager();

        let mut registry = AgentRegistry::new(provider_registry, tools);
        registry.set_dynamic_config(dcm);

        // Register a TOML agent with the same name as TOON - TOML should take precedence
        registry.register(
            "toon-only-agent",
            AgentConfig {
                model: "default".to_string(),
                system_prompt: Some("TOML override".to_string()),
                tools: vec![],
                max_tool_iterations: 5,
                parallel_tools: false,
                extra: HashMap::new(),
                allowed_tools: None,
                compaction_enabled: None,
            },
        );

        let names = registry.agent_names();
        let count = names.iter().filter(|n| *n == "toon-only-agent").count();
        assert_eq!(count, 1, "Should not have duplicate agent names");
        assert!(registry.has_agent("toon-only-agent"));
    }

    #[test]
    fn test_get_agent_model_toon() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();
        let dcm = create_test_dynamic_config_manager();

        let registry = AgentRegistry::with_dynamic_config(
            create_test_agent_map(),
            provider_registry,
            tools,
            dcm,
        );

        // TOON-only agent
        assert_eq!(
            registry.get_agent_model("toon-only-agent"),
            Some("default".to_string())
        );
    }

    #[test]
    fn test_get_agent_tools_toon() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();
        let dcm = create_test_dynamic_config_manager();

        let registry = AgentRegistry::with_dynamic_config(
            create_test_agent_map(),
            provider_registry,
            tools,
            dcm,
        );

        // TOON-only agent
        let tools = registry.get_agent_tools("toon-only-agent");
        assert_eq!(tools, vec!["calculator".to_string()]);
    }

    #[test]
    fn test_get_agent_system_prompt_toon() {
        let provider_registry = create_test_provider_registry();
        let tools = empty_tools();
        let dcm = create_test_dynamic_config_manager();

        let registry = AgentRegistry::with_dynamic_config(
            create_test_agent_map(),
            provider_registry,
            tools,
            dcm,
        );

        // TOON-only agent system prompt
        assert_eq!(
            registry.get_agent_system_prompt("toon-only-agent"),
            Some("TOON system prompt".to_string())
        );
    }
}

impl cordis::Service for AgentRegistry {
    fn name(&self) -> &'static str { "agent_registry" }
    fn init(&self, _ctx: &std::sync::Arc<cordis::Context>) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool { true }
}

