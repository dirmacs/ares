//! Integration tests for TOML configuration system
//!
//! These tests verify that the configuration system works end-to-end:
//! - Configuration loading and validation
//! - Registry creation from config
//! - Agent creation from registry
//! - Workflow execution
#![cfg(feature = "http")]

use std::collections::HashMap;
use std::sync::Arc;

/// Test helper: Create a minimal valid configuration
fn create_test_config() -> ares_http::overlay::AresConfig {
    use ares_http::config::{AuthConfig as TomlAuthConfig, ServerConfig as TomlServerConfig};
    use ares_http::overlay::{DynamicConfigPaths, *};

    // Set required environment variables for validation
    // SAFETY: Tests should be run single-threaded for env var safety
    unsafe {
        std::env::set_var("TEST_JWT_SECRET", "test-jwt-secret-at-least-32-chars");
        std::env::set_var("TEST_API_KEY", "test-api-key");
    }

    let mut providers = HashMap::new();
    providers.insert(
        "test-ollama".to_string(),
        ProviderConfig::OpenAI {
            api_key_env: "TEST_KEY".to_string(),
            api_base: "https://test.example.com/v1".to_string(),
            default_model: "ministral-3:3b".to_string(),
        },
    );

    let mut models = HashMap::new();
    models.insert(
        "test-model".to_string(),
        ModelConfig {
            provider: "test-ollama".to_string(),
            model: "ministral-3:3b".to_string(),
            temperature: 0.7,
            max_tokens: 512,
        },
    );

    let mut tools = HashMap::new();
    tools.insert(
        "calculator".to_string(),
        ToolConfig {
            enabled: true,
            description: Some("Calculator tool".to_string()),
            timeout_secs: 10,
            extra: HashMap::new(),
        },
    );

    let mut agents = HashMap::new();
    agents.insert(
        "test-agent".to_string(),
        AgentConfig {
            model: "test-model".to_string(),
            tools: vec!["calculator".to_string()],
            allowed_tools: None,
            system_prompt: Some("You are a test agent.".to_string()),
            max_tool_iterations: 10,
            parallel_tools: false,
            extra: HashMap::new(),
            compaction_enabled: None,
        },
    );
    agents.insert(
        "fallback-agent".to_string(),
        AgentConfig {
            model: "test-model".to_string(),
            tools: vec![],
            allowed_tools: None,
            system_prompt: Some("You are a fallback agent.".to_string()),
            max_tool_iterations: 10,
            parallel_tools: false,
            extra: HashMap::new(),
            compaction_enabled: None,
        },
    );

    let mut workflows = HashMap::new();
    workflows.insert(
        "test-workflow".to_string(),
        WorkflowConfig {
            entry_agent: "test-agent".to_string(),
            fallback_agent: Some("fallback-agent".to_string()),
            max_depth: 5,
            max_iterations: 5,
            parallel_subagents: false,
        },
    );

    AresConfig {
        server: TomlServerConfig::default(),
        auth: TomlAuthConfig {
            jwt_secret_env: "TEST_JWT_SECRET".to_string(),
            jwt_access_expiry: 900,
            jwt_refresh_expiry: 604800,
            api_key_env: "TEST_API_KEY".to_string(),
        },
        database: DatabaseConfig::default(),
        nvidia: None,
        config: DynamicConfigPaths::default(),
        providers,
        models,
        tools,
        agents,
        workflows,
        rag: RagConfig::default(),
        billing: BillingConfig {
            model_pricing: HashMap::new(),
        },
        skills: None,
    }
}

#[test]
fn test_config_creation_and_validation() {
    let config = create_test_config();

    // Should validate successfully
    let result = config.validate();
    assert!(result.is_ok(), "Config validation failed: {:?}", result);
}

#[test]
fn test_config_with_warnings() {
    use ares_http::overlay::*;

    let mut config = create_test_config();

    // Add an unused provider
    config.providers.insert(
        "unused-provider".to_string(),
        ProviderConfig::OpenAI {
            api_key_env: "TEST_KEY".to_string(),
            api_base: "https://test.example.com/v1".to_string(),
            default_model: "unused".to_string(),
        },
    );

    // Validation should pass but return warnings
    let warnings = config.validate_with_warnings().expect("Validation failed");

    assert!(
        warnings
            .iter()
            .any(|w| w.message.contains("unused-provider")),
        "Expected warning about unused provider"
    );
}

#[test]
fn test_agent_registry_from_config() {
    use ares_agent::AgentRegistry;
    use ares_llm::ProviderRegistry;
    use ares_tools::Tools;

    let config = create_test_config();

    // Create registries
    let provider_registry = Arc::new(ProviderRegistry::from_config(
        config.providers.clone(),
        config.models.clone(),
        config.nvidia.as_ref(),
    ));
    let tool_registry = Arc::new(Tools::from_static([]));

    // Create agent registry from config
    let agent_registry = AgentRegistry::from_config(
        config.agents.clone(),
        provider_registry,
        tool_registry.clone(),
    );

    // Verify agents are registered
    assert!(agent_registry.has_agent("test-agent"));
    assert!(agent_registry.has_agent("fallback-agent"));
    assert!(!agent_registry.has_agent("nonexistent"));

    // Verify agent configuration
    let model = agent_registry.get_agent_model("test-agent");
    assert_eq!(model, Some("test-model".to_string()));

    let tools = agent_registry.get_agent_tools("test-agent");
    assert!(tools.contains(&"calculator".to_string()));
}

#[test]
fn test_provider_registry_from_config() {
    use ares_llm::ProviderRegistry;

    let config = create_test_config();
    let registry = ProviderRegistry::from_config(
        config.providers.clone(),
        config.models.clone(),
        config.nvidia.as_ref(),
    );

    // Should have the test provider registered
    assert!(registry.has_model("test-model"));
}

#[tokio::test]
async fn test_workflow_engine_from_config() {
    use ares_agent::workflows::WorkflowEngine;
    use ares_agent::AgentRegistry;
    use ares_http::{AresConfigManager, DynamicConfigManager};
    use ares_llm::ConfigBasedLLMFactory;
    use ares_llm::ProviderRegistry;
    use ares_tools::Tools;
    use cordis::Context;

    let config = create_test_config();

    // Create registries
    let provider_registry = Arc::new(ProviderRegistry::from_config(
        config.providers.clone(),
        config.models.clone(),
        config.nvidia.as_ref(),
    ));
    let tool_registry = Arc::new(Tools::from_static([]));
    let agent_registry = Arc::new(AgentRegistry::from_config(
        config.agents.clone(),
        provider_registry.clone(),
        tool_registry.clone(),
    ));

    let config_manager = Arc::new(AresConfigManager::from_config(config));
    let llm_factory = Arc::new(ConfigBasedLLMFactory::new(
        provider_registry.clone(),
        "test-model",
    ));

    let dynamic_config = Arc::new(
        DynamicConfigManager::new(
            std::path::PathBuf::from("config/agents"),
            std::path::PathBuf::from("config/models"),
            std::path::PathBuf::from("config/tools"),
            std::path::PathBuf::from("config/workflows"),
            std::path::PathBuf::from("config/mcps"),
            false,
        )
        .unwrap(),
    );
    let db = Arc::new(ares_store::PostgresClient::new_test());
    let tenant_db = Arc::new(ares_store::TenantDb::new(Arc::new(
        ares_store::PostgresClient::new_test(),
    )));
    let auth_service = Arc::new(ares_http::auth::jwt::AuthService::new(
        "secret".to_string(),
        900,
        604800,
    ));
    let llm = Arc::new(
        ares_llm::Llm::new(
            provider_registry.clone(),
            Arc::new(ares_llm::ClientPool::with_defaults()),
            None,
        )
        .with_factory(llm_factory.clone()),
    );
    let skill_engine = Arc::new(ares_agent::skills::SkillEngine::new(
        tenant_db.pool().clone(),
        tool_registry.clone(),
        llm,
    ));

    let state: Arc<Context> = Context::new_root();
    state.provide_arc(config_manager.clone());
    state.provide_arc(dynamic_config);
    state.provide_arc(db.clone());
    state.provide_arc(tenant_db.clone());
    state.provide_arc(llm_factory.clone());
    state.provide_arc(provider_registry.clone());
    state.provide_arc(agent_registry);
    state.provide_arc(tool_registry.clone());
    state.provide_arc(auth_service.clone());
    #[cfg(feature = "mcp")]
    state.provide(ares_http::api::handlers::deploy::DeployRegistry::default());
    state.provide(ares_http::api::handlers::loops::LoopRegistry::new());
    state.provide(ares_agent::EmergencyStop::new(false));
    state.provide(ares_agent::ContextProviderHandle::new(std::sync::Arc::new(
        ares_agent::context_provider::NoOpContextProvider,
    )));
    state.provide(ares_store::FleetSecrets::new());
    state.provide(ares_http::active_runs::ActiveRuns::new());
    state.provide_arc(skill_engine);

    // Create workflow engine — feed the config's workflows the way the
    // prod handler does (AresConfigManager.config().workflows).
    let engine =
        WorkflowEngine::with_config(state.clone(), config_manager.config().workflows.clone());

    // Verify workflow is available
    let workflows = engine.available_workflows();
    assert!(workflows.iter().any(|w| *w == "test-workflow"));

    // Verify workflow config
    let wf_config = engine.get_workflow_config("test-workflow");
    assert!(wf_config.is_some());
    let wf = wf_config.unwrap();
    assert_eq!(wf.entry_agent, "test-agent");
    assert_eq!(wf.fallback_agent, Some("fallback-agent".to_string()));
}

#[test]
fn test_circular_reference_rejected() {
    use ares_http::overlay::*;

    let mut config = create_test_config();

    // Create a workflow with circular fallback (entry == fallback)
    config.workflows.insert(
        "circular".to_string(),
        WorkflowConfig {
            entry_agent: "test-agent".to_string(),
            fallback_agent: Some("test-agent".to_string()), // Same as entry!
            max_depth: 5,
            max_iterations: 5,
            parallel_subagents: false,
        },
    );

    let result = config.validate();
    assert!(result.is_err(), "Should reject circular reference");

    if let Err(ConfigError::CircularReference(msg)) = result {
        assert!(
            msg.contains("circular"),
            "Error should mention circular reference"
        );
    } else {
        panic!("Expected CircularReference error");
    }
}

#[test]
fn test_missing_reference_rejected() {
    use ares_http::overlay::*;

    let mut config = create_test_config();

    // Add agent referencing nonexistent model
    config.agents.insert(
        "broken-agent".to_string(),
        AgentConfig {
            model: "nonexistent-model".to_string(),
            tools: vec![],
            allowed_tools: None,
            system_prompt: None,
            max_tool_iterations: 10,
            parallel_tools: false,
            extra: HashMap::new(),
            compaction_enabled: None,
        },
    );

    // Since the dynamic NVIDIA catalog (2026-08-23), validate() only WARNS
    // about agent->model references not in the static [models] table — they
    // resolve against the live catalog at runtime. The strict guarantee moved
    // to validate_with_warnings-style checks; here we assert the config is
    // ACCEPTED and the reference survives for the runtime resolver.
    let result = config.validate();
    assert!(
        result.is_ok(),
        "missing static model reference must NOT fail validation (runtime-resolved)"
    );
    assert_eq!(
        config.agents["broken-agent"].model, "nonexistent-model",
        "the unresolved reference must survive for runtime resolution"
    );
}

#[test]
fn test_tool_filtering_in_agent() {
    use ares_http::overlay::AgentConfig;

    // Agent with restricted tools
    let agent_config = AgentConfig {
        model: "test-model".to_string(),
        tools: vec!["calculator".to_string()],
        allowed_tools: None,
        system_prompt: None,
        max_tool_iterations: 10,
        parallel_tools: false,
        extra: HashMap::new(),
        compaction_enabled: None,
    };

    // Verify tools are captured
    assert_eq!(agent_config.tools.len(), 1);
    assert!(agent_config.tools.contains(&"calculator".to_string()));
    assert!(!agent_config.tools.contains(&"web_search".to_string()));
}

#[test]
fn test_config_manager_access() {
    use ares_http::overlay::AresConfigManager;

    let config = create_test_config();
    let manager = AresConfigManager::from_config(config.clone());

    // Get config through manager
    let loaded = manager.config();

    // Verify data matches
    assert_eq!(loaded.server.host, config.server.host);
    assert_eq!(loaded.server.port, config.server.port);
    assert!(loaded.agents.contains_key("test-agent"));
}

#[test]
fn test_full_integration_config_to_agent() {
    use ares_agent::AgentRegistry;
    use ares_llm::ProviderRegistry;
    use ares_tools::Tools;

    let config = create_test_config();

    // Create full stack of registries
    let provider_registry = Arc::new(ProviderRegistry::from_config(
        config.providers.clone(),
        config.models.clone(),
        config.nvidia.as_ref(),
    ));
    let tool_registry = Arc::new(Tools::from_static([]));
    let agent_registry = AgentRegistry::from_config(
        config.agents.clone(),
        provider_registry.clone(),
        tool_registry.clone(),
    );

    // Verify the full chain works
    // 1. Config has agent
    assert!(config.agents.contains_key("test-agent"));

    // 2. Agent references valid model
    let agent_config = config.agents.get("test-agent").unwrap();
    assert!(config.models.contains_key(&agent_config.model));

    // 3. Model references valid provider
    let model_config = config.models.get(&agent_config.model).unwrap();
    assert!(config.providers.contains_key(&model_config.provider));

    // 4. Registry has agent
    assert!(agent_registry.has_agent("test-agent"));

    // 5. Registry can provide agent model
    assert_eq!(
        agent_registry.get_agent_model("test-agent"),
        Some("test-model".to_string())
    );
}
