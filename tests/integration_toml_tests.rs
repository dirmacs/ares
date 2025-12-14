//! Integration tests for TOML configuration system
//!
//! These tests verify that the configuration system works end-to-end:
//! - Configuration loading and validation
//! - Registry creation from config
//! - Agent creation from registry
//! - Workflow execution

use std::collections::HashMap;
use std::sync::Arc;

/// Test helper: Create a minimal valid configuration
fn create_test_config() -> ares::utils::toml_config::AresConfig {
    use ares::utils::toml_config::*;

    // Set required environment variables for validation
    // SAFETY: Tests should be run single-threaded for env var safety
    unsafe {
        std::env::set_var("TEST_JWT_SECRET", "test-jwt-secret-at-least-32-chars");
        std::env::set_var("TEST_API_KEY", "test-api-key");
    }

    let mut providers = HashMap::new();
    providers.insert(
        "test-ollama".to_string(),
        ProviderConfig::Ollama {
            base_url: "http://localhost:11434".to_string(),
            default_model: "granite4:tiny-h".to_string(),
        },
    );

    let mut models = HashMap::new();
    models.insert(
        "test-model".to_string(),
        ModelConfig {
            provider: "test-ollama".to_string(),
            model: "granite4:tiny-h".to_string(),
            temperature: 0.7,
            max_tokens: 512,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
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
            system_prompt: Some("You are a test agent.".to_string()),
            max_tool_iterations: 10,
            parallel_tools: false,
            extra: HashMap::new(),
        },
    );
    agents.insert(
        "fallback-agent".to_string(),
        AgentConfig {
            model: "test-model".to_string(),
            tools: vec![],
            system_prompt: Some("You are a fallback agent.".to_string()),
            max_tool_iterations: 10,
            parallel_tools: false,
            extra: HashMap::new(),
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
        server: ServerConfig::default(),
        auth: AuthConfig {
            jwt_secret_env: "TEST_JWT_SECRET".to_string(),
            jwt_access_expiry: 900,
            jwt_refresh_expiry: 604800,
            api_key_env: "TEST_API_KEY".to_string(),
        },
        database: DatabaseConfig::default(),
        providers,
        models,
        tools,
        agents,
        workflows,
        rag: RagConfig::default(),
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
    use ares::utils::toml_config::*;
    
    let mut config = create_test_config();
    
    // Add an unused provider
    config.providers.insert(
        "unused-provider".to_string(),
        ProviderConfig::Ollama {
            base_url: "http://localhost:11434".to_string(),
            default_model: "unused".to_string(),
        },
    );
    
    // Validation should pass but return warnings
    let warnings = config.validate_with_warnings().expect("Validation failed");
    
    assert!(
        warnings.iter().any(|w| w.message.contains("unused-provider")),
        "Expected warning about unused provider"
    );
}

#[test]
fn test_agent_registry_from_config() {
    use ares::agents::AgentRegistry;
    use ares::llm::ProviderRegistry;
    use ares::tools::registry::ToolRegistry;
    
    let config = create_test_config();
    
    // Create registries
    let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
    let tool_registry = Arc::new(ToolRegistry::new());
    
    // Create agent registry from config
    let agent_registry = AgentRegistry::from_config(&config, provider_registry, tool_registry);
    
    // Verify agents are registered
    assert!(agent_registry.has_agent("test-agent"));
    assert!(agent_registry.has_agent("fallback-agent"));
    assert!(!agent_registry.has_agent("nonexistent"));
    
    // Verify agent configuration
    let model = agent_registry.get_agent_model("test-agent");
    assert_eq!(model, Some("test-model"));
    
    let tools = agent_registry.get_agent_tools("test-agent");
    assert!(tools.contains(&"calculator"));
}

#[test]
fn test_provider_registry_from_config() {
    use ares::llm::ProviderRegistry;
    
    let config = create_test_config();
    let registry = ProviderRegistry::from_config(&config);
    
    // Should have the test provider registered
    assert!(registry.has_model("test-model"));
}

#[test]
fn test_workflow_engine_from_config() {
    use ares::workflows::WorkflowEngine;
    use ares::agents::AgentRegistry;
    use ares::llm::ProviderRegistry;
    use ares::tools::registry::ToolRegistry;
    
    let config = create_test_config();
    
    // Create registries
    let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
    let tool_registry = Arc::new(ToolRegistry::new());
    let agent_registry = Arc::new(AgentRegistry::from_config(
        &config,
        provider_registry,
        tool_registry,
    ));
    
    // Create config Arc
    let config_arc = Arc::new(config);
    
    // Create workflow engine
    let engine = WorkflowEngine::new(agent_registry, config_arc);
    
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
    use ares::utils::toml_config::*;
    
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
    assert!(
        result.is_err(),
        "Should reject circular reference"
    );
    
    if let Err(ConfigError::CircularReference(msg)) = result {
        assert!(msg.contains("circular"), "Error should mention circular reference");
    } else {
        panic!("Expected CircularReference error");
    }
}

#[test]
fn test_missing_reference_rejected() {
    use ares::utils::toml_config::*;
    
    let mut config = create_test_config();
    
    // Add agent referencing nonexistent model
    config.agents.insert(
        "broken-agent".to_string(),
        AgentConfig {
            model: "nonexistent-model".to_string(),
            tools: vec![],
            system_prompt: None,
            max_tool_iterations: 10,
            parallel_tools: false,
            extra: HashMap::new(),
        },
    );
    
    let result = config.validate();
    assert!(result.is_err(), "Should reject missing model reference");
    
    match result {
        Err(ConfigError::MissingModel(model, agent)) => {
            assert_eq!(model, "nonexistent-model");
            assert_eq!(agent, "broken-agent");
        }
        _ => panic!("Expected MissingModel error"),
    }
}

#[test]
fn test_tool_filtering_in_agent() {
    use ares::utils::toml_config::AgentConfig;
    
    // Agent with restricted tools
    let agent_config = AgentConfig {
        model: "test-model".to_string(),
        tools: vec!["calculator".to_string()],
        system_prompt: None,
        max_tool_iterations: 10,
        parallel_tools: false,
        extra: HashMap::new(),
    };
    
    // Verify tools are captured
    assert_eq!(agent_config.tools.len(), 1);
    assert!(agent_config.tools.contains(&"calculator".to_string()));
    assert!(!agent_config.tools.contains(&"web_search".to_string()));
}

#[test]
fn test_config_manager_access() {
    use ares::utils::toml_config::AresConfigManager;
    
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
    use ares::agents::AgentRegistry;
    use ares::llm::ProviderRegistry;
    use ares::tools::registry::ToolRegistry;
    
    let config = create_test_config();
    
    // Create full stack of registries
    let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
    let tool_registry = Arc::new(ToolRegistry::new());
    let agent_registry = AgentRegistry::from_config(
        &config,
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
        Some("test-model")
    );
}
