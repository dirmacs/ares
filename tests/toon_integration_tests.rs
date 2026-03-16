//! Integration tests for TOON-based dynamic configuration system
//!
//! These tests verify that:
//! - TOON config files can be loaded from directories
//! - DynamicConfigManager provides correct access to configs
//! - TOON format serialization/deserialization works correctly

use ares::utils::toon_config::{
    DynamicConfigManager, ToonAgentConfig, ToonModelConfig, ToonToolConfig, ToonWorkflowConfig,
};
use std::path::PathBuf;
use tempfile::TempDir;
use toon_format::{decode_default, encode_default};

/// Helper to create a temp directory with TOON config files
fn setup_test_config_dirs() -> (TempDir, PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let base = temp_dir.path();

    let agents_dir = base.join("agents");
    let models_dir = base.join("models");
    let tools_dir = base.join("tools");
    let workflows_dir = base.join("workflows");
    let mcps_dir = base.join("mcps");

    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&models_dir).unwrap();
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::create_dir_all(&workflows_dir).unwrap();
    std::fs::create_dir_all(&mcps_dir).unwrap();

    (
        temp_dir,
        agents_dir,
        models_dir,
        tools_dir,
        workflows_dir,
        mcps_dir,
    )
}

#[test]
fn test_toon_agent_roundtrip() {
    let agent = ToonAgentConfig {
        name: "test-agent".to_string(),
        model: "gpt-4".to_string(),
        system_prompt: Some("You are a helpful assistant.".to_string()),
        tools: vec!["calculator".to_string(), "web_search".to_string()],
        max_tool_iterations: 10,
        parallel_tools: true,
        extra: std::collections::HashMap::new(),
    };

    // Encode to TOON
    let toon = encode_default(&agent).expect("Failed to encode agent");

    // Decode back
    let decoded: ToonAgentConfig = decode_default(&toon).expect("Failed to decode agent");

    assert_eq!(agent.name, decoded.name);
    assert_eq!(agent.model, decoded.model);
    assert_eq!(agent.system_prompt, decoded.system_prompt);
    assert_eq!(agent.tools, decoded.tools);
    assert_eq!(agent.max_tool_iterations, decoded.max_tool_iterations);
    assert_eq!(agent.parallel_tools, decoded.parallel_tools);
}

#[test]
fn test_toon_model_roundtrip() {
    let model = ToonModelConfig {
        name: "gpt-4-turbo".to_string(),
        provider: "openai".to_string(),
        model: "gpt-4-turbo-preview".to_string(),
        max_tokens: 4096,
        temperature: 0.7,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
    };

    let toon = encode_default(&model).expect("Failed to encode model");
    let decoded: ToonModelConfig = decode_default(&toon).expect("Failed to decode model");

    assert_eq!(model.name, decoded.name);
    assert_eq!(model.provider, decoded.provider);
    assert_eq!(model.model, decoded.model);
    assert_eq!(model.max_tokens, decoded.max_tokens);
    assert_eq!(model.temperature, decoded.temperature);
}

#[test]
fn test_toon_tool_roundtrip() {
    let tool = ToonToolConfig {
        name: "web_search".to_string(),
        enabled: true,
        description: Some("Search the web".to_string()),
        timeout_secs: 30,
        extra: std::collections::HashMap::new(),
    };

    let toon = encode_default(&tool).expect("Failed to encode tool");
    let decoded: ToonToolConfig = decode_default(&toon).expect("Failed to decode tool");

    assert_eq!(tool.name, decoded.name);
    assert_eq!(tool.enabled, decoded.enabled);
    assert_eq!(tool.timeout_secs, decoded.timeout_secs);
}

#[test]
fn test_toon_workflow_roundtrip() {
    let workflow = ToonWorkflowConfig {
        name: "research".to_string(),
        entry_agent: "researcher".to_string(),
        fallback_agent: Some("general".to_string()),
        max_depth: 5,
        max_iterations: 20,
        parallel_subagents: true,
    };

    let toon = encode_default(&workflow).expect("Failed to encode workflow");
    let decoded: ToonWorkflowConfig = decode_default(&toon).expect("Failed to decode workflow");

    assert_eq!(workflow.name, decoded.name);
    assert_eq!(workflow.entry_agent, decoded.entry_agent);
    assert_eq!(workflow.fallback_agent, decoded.fallback_agent);
    assert_eq!(workflow.max_depth, decoded.max_depth);
}

#[test]
fn test_dynamic_config_manager_load_from_dirs() {
    let (_temp_dir, agents_dir, models_dir, tools_dir, workflows_dir, mcps_dir) =
        setup_test_config_dirs();

    // Create a test agent TOON file
    let agent = ToonAgentConfig {
        name: "test-agent".to_string(),
        model: "test-model".to_string(),
        system_prompt: Some("Test system prompt".to_string()),
        tools: vec![],
        max_tool_iterations: 5,
        parallel_tools: false,
        extra: std::collections::HashMap::new(),
    };
    let agent_toon = encode_default(&agent).expect("Failed to encode agent");
    std::fs::write(agents_dir.join("test-agent.toon"), agent_toon).unwrap();

    // Create a test model TOON file
    let model = ToonModelConfig {
        name: "test-model".to_string(),
        provider: "test-provider".to_string(),
        model: "test-api-model".to_string(),
        max_tokens: 1000,
        temperature: 0.7,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
    };
    let model_toon = encode_default(&model).expect("Failed to encode model");
    std::fs::write(models_dir.join("test-model.toon"), model_toon).unwrap();

    // Create the manager
    let manager = DynamicConfigManager::new(
        agents_dir,
        models_dir,
        tools_dir,
        workflows_dir,
        mcps_dir,
        false, // No hot reload for test
    )
    .expect("Failed to create DynamicConfigManager");

    // Verify configs were loaded
    assert!(manager.agent("test-agent").is_some());
    assert!(manager.model("test-model").is_some());

    let loaded_agent = manager.agent("test-agent").unwrap();
    assert_eq!(loaded_agent.name, "test-agent");
    assert_eq!(loaded_agent.model, "test-model");

    let loaded_model = manager.model("test-model").unwrap();
    assert_eq!(loaded_model.name, "test-model");
    assert_eq!(loaded_model.provider, "test-provider");
}

#[test]
fn test_dynamic_config_manager_list_methods() {
    let (_temp_dir, agents_dir, models_dir, tools_dir, workflows_dir, mcps_dir) =
        setup_test_config_dirs();

    // Create multiple agents
    for i in 1..=3 {
        let agent = ToonAgentConfig {
            name: format!("agent-{}", i),
            model: "model".to_string(),
            system_prompt: None,
            tools: vec![],
            max_tool_iterations: 5,
            parallel_tools: false,
            extra: std::collections::HashMap::new(),
        };
        let toon = encode_default(&agent).expect("Failed to encode");
        std::fs::write(agents_dir.join(format!("agent-{}.toon", i)), toon).unwrap();
    }

    // Create multiple models
    for i in 1..=2 {
        let model = ToonModelConfig {
            name: format!("model-{}", i),
            provider: "provider".to_string(),
            model: "api-model".to_string(),
            max_tokens: 1000,
            temperature: 0.7,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
        };
        let toon = encode_default(&model).expect("Failed to encode");
        std::fs::write(models_dir.join(format!("model-{}.toon", i)), toon).unwrap();
    }

    let manager = DynamicConfigManager::new(
        agents_dir,
        models_dir,
        tools_dir,
        workflows_dir,
        mcps_dir,
        false,
    )
    .expect("Failed to create DynamicConfigManager");

    // Test list methods
    assert_eq!(manager.agents().len(), 3);
    assert_eq!(manager.models().len(), 2);
    assert_eq!(manager.tools().len(), 0);
    assert_eq!(manager.workflows().len(), 0);

    // Test name list methods
    let agent_names = manager.agent_names();
    assert_eq!(agent_names.len(), 3);
    assert!(agent_names.contains(&"agent-1".to_string()));
    assert!(agent_names.contains(&"agent-2".to_string()));
    assert!(agent_names.contains(&"agent-3".to_string()));
}

#[test]
fn test_empty_directories_work() {
    let (_temp_dir, agents_dir, models_dir, tools_dir, workflows_dir, mcps_dir) =
        setup_test_config_dirs();

    // Don't create any config files - directories are empty

    let manager = DynamicConfigManager::new(
        agents_dir,
        models_dir,
        tools_dir,
        workflows_dir,
        mcps_dir,
        false,
    )
    .expect("Failed to create DynamicConfigManager with empty dirs");

    assert_eq!(manager.agents().len(), 0);
    assert_eq!(manager.models().len(), 0);
    assert_eq!(manager.tools().len(), 0);
    assert!(manager.agent("nonexistent").is_none());
}

#[test]
fn test_toon_mcp_roundtrip() {
    use ares::utils::toon_config::ToonMcpConfig;

    let mcp = ToonMcpConfig {
        name: "filesystem".to_string(),
        enabled: true,
        command: "npx".to_string(),
        args: vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-filesystem".to_string(),
        ],
        env: {
            let mut env = std::collections::HashMap::new();
            env.insert("NODE_ENV".to_string(), "production".to_string());
            env
        },
        timeout_secs: 30,
    };

    let toon = encode_default(&mcp).expect("Failed to encode MCP config");
    let decoded: ToonMcpConfig = decode_default(&toon).expect("Failed to decode MCP config");

    assert_eq!(mcp.name, decoded.name);
    assert_eq!(mcp.enabled, decoded.enabled);
    assert_eq!(mcp.command, decoded.command);
    assert_eq!(mcp.args, decoded.args);
    assert_eq!(mcp.env, decoded.env);
    assert_eq!(mcp.timeout_secs, decoded.timeout_secs);
}

#[test]
fn test_toon_config_with_extra_fields() {
    // Test that extra/unknown fields in TOON are preserved via serde flatten
    let agent = ToonAgentConfig {
        name: "agent-with-extra".to_string(),
        model: "test-model".to_string(),
        system_prompt: Some("Test prompt".to_string()),
        tools: vec![],
        max_tool_iterations: 5,
        parallel_tools: false,
        extra: {
            let mut extra = std::collections::HashMap::new();
            extra.insert(
                "custom_field".to_string(),
                serde_json::json!("custom_value"),
            );
            extra.insert("custom_number".to_string(), serde_json::json!(42));
            extra
        },
    };

    let toon = encode_default(&agent).expect("Failed to encode agent with extra fields");
    let decoded: ToonAgentConfig =
        decode_default(&toon).expect("Failed to decode agent with extra fields");

    assert_eq!(agent.name, decoded.name);
    assert_eq!(
        agent.extra.get("custom_field"),
        decoded.extra.get("custom_field")
    );
    assert_eq!(
        agent.extra.get("custom_number"),
        decoded.extra.get("custom_number")
    );
}

#[test]
fn test_toon_model_with_optional_fields() {
    let model = ToonModelConfig {
        name: "model-with-optionals".to_string(),
        provider: "openai".to_string(),
        model: "gpt-4".to_string(),
        max_tokens: 8192,
        temperature: 0.0,
        top_p: Some(0.95),
        frequency_penalty: Some(0.5),
        presence_penalty: Some(-0.5),
    };

    let toon = encode_default(&model).expect("Failed to encode model with optionals");
    let decoded: ToonModelConfig =
        decode_default(&toon).expect("Failed to decode model with optionals");

    assert_eq!(model.name, decoded.name);
    assert_eq!(model.top_p, decoded.top_p);
    assert_eq!(model.frequency_penalty, decoded.frequency_penalty);
    assert_eq!(model.presence_penalty, decoded.presence_penalty);
}

#[test]
fn test_dynamic_config_validation_with_workflows() {
    let (_temp_dir, agents_dir, models_dir, tools_dir, workflows_dir, mcps_dir) =
        setup_test_config_dirs();

    // Create model
    let model = ToonModelConfig {
        name: "test-model".to_string(),
        provider: "test-provider".to_string(),
        model: "test-api-model".to_string(),
        max_tokens: 1000,
        temperature: 0.7,
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
    };
    std::fs::write(
        models_dir.join("test-model.toon"),
        encode_default(&model).unwrap(),
    )
    .unwrap();

    // Create agent
    let agent = ToonAgentConfig {
        name: "router".to_string(),
        model: "test-model".to_string(),
        system_prompt: Some("Router agent".to_string()),
        tools: vec![],
        max_tool_iterations: 5,
        parallel_tools: false,
        extra: std::collections::HashMap::new(),
    };
    std::fs::write(
        agents_dir.join("router.toon"),
        encode_default(&agent).unwrap(),
    )
    .unwrap();

    // Create workflow referencing the agent
    let workflow = ToonWorkflowConfig {
        name: "default".to_string(),
        entry_agent: "router".to_string(),
        fallback_agent: None,
        max_depth: 3,
        max_iterations: 5,
        parallel_subagents: false,
    };
    std::fs::write(
        workflows_dir.join("default.toon"),
        encode_default(&workflow).unwrap(),
    )
    .unwrap();

    let manager = DynamicConfigManager::new(
        agents_dir,
        models_dir,
        tools_dir,
        workflows_dir,
        mcps_dir,
        false,
    )
    .expect("Failed to create DynamicConfigManager");

    // Validation should pass
    let config = manager.config();
    let result = config.validate();
    assert!(result.is_ok(), "Validation failed: {:?}", result.err());
}

#[test]
fn test_dynamic_config_validation_invalid_workflow_agent() {
    let (_temp_dir, agents_dir, models_dir, tools_dir, workflows_dir, mcps_dir) =
        setup_test_config_dirs();

    // Create workflow referencing non-existent agent
    let workflow = ToonWorkflowConfig {
        name: "invalid-workflow".to_string(),
        entry_agent: "nonexistent-agent".to_string(),
        fallback_agent: None,
        max_depth: 3,
        max_iterations: 5,
        parallel_subagents: false,
    };
    std::fs::write(
        workflows_dir.join("invalid-workflow.toon"),
        encode_default(&workflow).unwrap(),
    )
    .unwrap();

    let manager = DynamicConfigManager::new(
        agents_dir,
        models_dir,
        tools_dir,
        workflows_dir,
        mcps_dir,
        false,
    )
    .expect("Failed to create DynamicConfigManager");

    // Validation should fail
    let config = manager.config();
    let result = config.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("unknown"));
}
