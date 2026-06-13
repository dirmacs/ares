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
use ares_llm::ProviderRegistry;
use ares_tools::registry::ToolRegistry;
use ares_types::types::{AgentType, AppError, Result};
use ares_config::toml_config::{AgentConfig, AresConfig};
use ares_config::toon_config::{DynamicConfigManager, ToonAgentConfig};
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
            allowed_tools: toon.allowed_tools.clone(),
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
        let provider_name = self
            .provider_registry
            .get_model(&config.model)
            .map(|model| model.provider.clone())
            .unwrap_or_else(|| config.model.clone());

        // Pass the full tool registry; the agent will filter based on allowed_tools
        let agent_tool_registry = Some(Arc::clone(&self.tool_registry));

        Ok(ConfigurableAgent::new_with_provider(
            name,
            config,
            llm,
            agent_tool_registry,
            provider_name,
        ))
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
        fleet_secrets: &ares_config::fleet_secrets::FleetSecrets,
    ) -> Result<ConfigurableAgent> {
        let chain = self
            .provider_registry
            .resolve_with_fallback(&config.model, tenant_id, pool, fleet_secrets)
            .await;

        let mut iter = chain.into_iter();
        let (primary_provider, _) = iter.next().ok_or_else(|| {
            AppError::Configuration(format!(
                "No provider resolved for model/tier '{}'",
                config.model
            ))
        })?;

        let llm = self
            .provider_registry
            .create_client_for_provider(&primary_provider)
            .await?;

        let mut fallback_llms = Vec::new();
        for (provider_name, _) in iter {
            if let Ok(client) = self.provider_registry.create_client_for_provider(&provider_name).await {
                fallback_llms.push(client);
            }
        }

        let mut agent = ConfigurableAgent::new_with_provider(
            name,
            config,
            llm,
            Some(Arc::clone(&self.tool_registry)),
            primary_provider,
        );
        agent.set_fallback_llms(fallback_llms);

        // --- tenant allowlist enforcement ---
        let allowlist_store = ares_db::tenant_allowlist::TenantAllowlistStore::new(pool);

        // Model enforcement
        let resolved_model = {
            let tier_store = ares_db::tenant_model_tiers::TenantModelTierStore::new(pool);
            if let Ok(Some(tier)) = tier_store.get(tenant_id, &config.model).await {
                tier.model_name
            } else if let Some(model_cfg) = self.provider_registry.get_model(&config.model) {
                model_cfg.model.clone()
            } else {
                config.model.clone()
            }
        };
        if !allowlist_store
            .is_model_allowed(tenant_id, &resolved_model)
            .await
            .map_err(|e| AppError::Auth(format!("Failed to check model allowlist: {}", e)))?
        {
            return Err(AppError::Auth(format!(
                "Model '{}' is not allowed for this tenant",
                resolved_model
            )));
        }

        // Tool enforcement: intersect config allowed_tools with tenant allowlist
        let db_tools = allowlist_store
            .list_tools(tenant_id)
            .await
            .map_err(|e| AppError::Auth(format!("Failed to check tool allowlist: {}", e)))?;
        if !db_tools.is_empty() {
            let db_tool_names: Vec<String> = db_tools.iter().map(|t| t.tool_name.clone()).collect();
            let new_allowed = match agent.allowed_tools() {
                Some(agent_tools) => {
                    let intersect: Vec<String> = agent_tools
                        .iter()
                        .cloned()
                        .filter(|t| db_tool_names.contains(t))
                        .collect();
                    Some(intersect)
                }
                None => Some(db_tool_names),
            };
            agent.set_allowed_tools(new_allowed);
        }

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
    /// tools field.  An empty result means "all tools permitted" when
    /// allowed_tools is absent.
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
    use ares_config::toml_config::{ProviderConfig, ServerConfig, AuthConfig, DatabaseConfig, RagConfig, BillingConfig, DynamicConfigPaths};
    use std::collections::HashMap;

    fn create_test_ares_config() -> AresConfig {
        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            database: DatabaseConfig::default(),
            nvidia: None,
            providers: HashMap::new(),
            models: HashMap::new(),
            tools: HashMap::new(),
            agents: HashMap::new(),
            workflows: HashMap::new(),
            rag: RagConfig::default(),
            billing: BillingConfig::default(),
            config: DynamicConfigPaths::default(),
        }
    }

    fn create_test_ares_config_with_agents(agents: HashMap<String, AgentConfig>) -> AresConfig {
        let mut cfg = create_test_ares_config();
        cfg.agents = agents;
        cfg
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
            ares_config::toml_config::ModelConfig {
                provider: "ollama-local".to_string(),
                model: "ministral-3:3b".to_string(),
                temperature: 0.7,
                max_tokens: 512,
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
            allowed_tools: None,
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
                allowed_tools: None,
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
                allowed_tools: None,
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
                allowed_tools: None,
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
                    allowed_tools: None,
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
        let tool_registry = Arc::new(ToolRegistry::new());

        let ares_config = create_test_ares_config_with_agents({
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
},
            );
            map
        });

        let registry =
            AgentRegistry::from_config(&ares_config, provider_registry, tool_registry);

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

        let ares_config = create_test_ares_config_with_agents({
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
},
            );
            map
        });

        let result = AgentRegistryBuilder::new().from_config(&ares_config)
            .with_provider_registry(provider_registry)
            .build();

        assert!(result.is_ok());
        let registry = result.unwrap();
        assert!(registry.has_agent("builder-agent"));
    }

    // ============================================================
    //  AgentRegistryBuilder::with_tool_registry(...) and default tool registry
    // ============================================================

    #[test]
    fn test_builder_with_tool_registry_explicit() {
        let provider_registry = create_test_provider_registry();
        let custom_tool_registry = Arc::new(ToolRegistry::new());

        let result = AgentRegistryBuilder::new()
            .with_provider_registry(provider_registry)
            .with_tool_registry(custom_tool_registry.clone())
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
},
            )
            .build();

        assert!(result.is_ok());
        let registry = result.unwrap();
        assert!(registry.has_agent("tool-agent"));
    }

    #[test]
    fn test_builder_default_tool_registry() {
        let provider_registry = create_test_provider_registry();

        // Build without calling with_tool_registry - should use default ToolRegistry
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
        let tool_registry = Arc::new(ToolRegistry::new());
        let mut registry = AgentRegistry::new(provider_registry, tool_registry);

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
        let tool_registry = Arc::new(ToolRegistry::new());
        let mut registry = AgentRegistry::new(provider_registry, tool_registry);

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
        let tool_registry = Arc::new(ToolRegistry::new());
        let registry = AgentRegistry::new(provider_registry, tool_registry);

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
        let tool_registry = Arc::new(ToolRegistry::new());
        let registry = AgentRegistry::new(provider_registry, tool_registry);

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

    fn create_test_dynamic_config_manager() -> Arc<DynamicConfigManager> {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();

        let toon_cfg = ToonAgentConfig::new("toon-only-agent", "default")
            .with_system_prompt("TOON system prompt")
            .with_tools(vec!["calculator".to_string()]);
        let content = toon_cfg.to_toon().unwrap();
        std::fs::write(agents.join("a.toon"), content).unwrap();

        Arc::new(
            DynamicConfigManager::new(
                agents,
                dir.path().join("models"),
                dir.path().join("tools"),
                dir.path().join("workflows"),
                dir.path().join("mcps"),
                false, // hot_reload = false
            )
            .unwrap(),
        )
    }

    #[test]
    fn test_with_dynamic_config() {
        let provider_registry = create_test_provider_registry();
        let tool_registry = Arc::new(ToolRegistry::new());
        let dcm = create_test_dynamic_config_manager();

        let registry = AgentRegistry::with_dynamic_config(
            &create_test_ares_config(),
            provider_registry,
            tool_registry,
            dcm.clone(),
        );

        assert!(registry.get_toon_config("toon-only-agent").is_some());
        assert!(registry.has_agent("toon-only-agent"));
    }

    #[test]
    fn test_set_dynamic_config() {
        let provider_registry = create_test_provider_registry();
        let tool_registry = Arc::new(ToolRegistry::new());
        let dcm = create_test_dynamic_config_manager();

        let mut registry = AgentRegistry::new(provider_registry, tool_registry);
        assert!(!registry.has_agent("toon-only-agent")); // Not set yet

        registry.set_dynamic_config(dcm);
        assert!(registry.has_agent("toon-only-agent"));
        assert!(registry.get_toon_config("toon-only-agent").is_some());
    }

    #[test]
    fn test_toon_merge_agent_names_no_duplicates() {
        let provider_registry = create_test_provider_registry();
        let tool_registry = Arc::new(ToolRegistry::new());
        let dcm = create_test_dynamic_config_manager();

        let mut registry = AgentRegistry::new(provider_registry, tool_registry);
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
        let tool_registry = Arc::new(ToolRegistry::new());
        let dcm = create_test_dynamic_config_manager();

        let registry = AgentRegistry::with_dynamic_config(
            &create_test_ares_config(),
            provider_registry,
            tool_registry,
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
        let tool_registry = Arc::new(ToolRegistry::new());
        let dcm = create_test_dynamic_config_manager();

        let registry = AgentRegistry::with_dynamic_config(
            &create_test_ares_config(),
            provider_registry,
            tool_registry,
            dcm,
        );

        // TOON-only agent
        let tools = registry.get_agent_tools("toon-only-agent");
        assert_eq!(tools, vec!["calculator".to_string()]);
    }

    #[test]
    fn test_get_agent_system_prompt_toon() {
        let provider_registry = create_test_provider_registry();
        let tool_registry = Arc::new(ToolRegistry::new());
        let dcm = create_test_dynamic_config_manager();

        let registry = AgentRegistry::with_dynamic_config(
            &create_test_ares_config(),
            provider_registry,
            tool_registry,
            dcm,
        );

        // TOON-only agent system prompt
        assert_eq!(
            registry.get_agent_system_prompt("toon-only-agent"),
            Some("TOON system prompt".to_string())
        );
    }

    // ============================================================
    //  toon_to_agent_config - JSON value to TOML value conversion
    // ============================================================

    #[test]
    fn test_toon_to_agent_config_extra_conversion() {
        // Test String extra
        let toon_string = ToonAgentConfig::new("agent", "default").with_system_prompt("test");
        let mut toon_string = toon_string;
        toon_string.extra.insert(
            "string_key".to_string(),
            serde_json::Value::String("value".to_string()),
        );
        let agent_config = AgentRegistry::toon_to_agent_config(&toon_string);
        assert_eq!(
            agent_config.extra.get("string_key"),
            Some(&toml::Value::String("value".to_string()))
        );

        // Test Number (i64) extra
        let mut toon_i64 = ToonAgentConfig::new("agent", "default");
        toon_i64.extra.insert(
            "int_key".to_string(),
            serde_json::json!(42),
        );
        let agent_config = AgentRegistry::toon_to_agent_config(&toon_i64);
        assert_eq!(
            agent_config.extra.get("int_key"),
            Some(&toml::Value::Integer(42))
        );

        // Test Number (f64) extra
        let mut toon_f64 = ToonAgentConfig::new("agent", "default");
        toon_f64.extra.insert(
            "float_key".to_string(),
            serde_json::json!(3.14159),
        );
        let agent_config = AgentRegistry::toon_to_agent_config(&toon_f64);
        assert_eq!(
            agent_config.extra.get("float_key"),
            Some(&toml::Value::Float(3.14159))
        );

        // Test Bool extra
        let mut toon_bool = ToonAgentConfig::new("agent", "default");
        toon_bool.extra.insert(
            "bool_key".to_string(),
            serde_json::Value::Bool(true),
        );
        let agent_config = AgentRegistry::toon_to_agent_config(&toon_bool);
        assert_eq!(
            agent_config.extra.get("bool_key"),
            Some(&toml::Value::Boolean(true))
        );

        // Test Array/Object extra (converts to String)
        let mut toon_array = ToonAgentConfig::new("agent", "default");
        toon_array.extra.insert(
            "array_key".to_string(),
            serde_json::json!(vec![1, 2, 3]),
        );
        let agent_config = AgentRegistry::toon_to_agent_config(&toon_array);
        assert_eq!(
            agent_config.extra.get("array_key"),
            Some(&toml::Value::String("[1,2,3]".to_string()))
        );

        // Test Object extra (converts to String)
        let mut toon_object = ToonAgentConfig::new("agent", "default");
        toon_object.extra.insert(
            "object_key".to_string(),
            serde_json::json!({"key": "value"}),
        );
        let agent_config = AgentRegistry::toon_to_agent_config(&toon_object);
        assert_eq!(
            agent_config.extra.get("object_key"),
            Some(&toml::Value::String("{\"key\":\"value\"}".to_string()))
        );
    }
}
