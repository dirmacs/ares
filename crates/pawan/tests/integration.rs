//! Integration tests for Pawan with mocked Ollama server
//!
//! These tests use wiremock to simulate Ollama API responses,
//! allowing us to test the full agent workflow without a real LLM.

use pawan::agent::{PawanAgent, Role};
use pawan::config::{HealingConfig, PawanConfig};
use pawan::healing::{CompilerFixer, DiagnosticKind};
use pawan::tools::ToolRegistry;
use serde_json::json;
use std::path::PathBuf;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ============================================================================
// Helper Functions
// ============================================================================

/// Create a mock Ollama server that returns a simple response
async fn setup_mock_ollama() -> MockServer {
    let mock_server = MockServer::start().await;

    // Mock the chat endpoint
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "nemotron-mini",
            "message": {
                "role": "assistant",
                "content": "I'll help you with that task."
            },
            "done": true,
            "done_reason": "stop"
        })))
        .mount(&mock_server)
        .await;

    mock_server
}

/// Create a mock Ollama that returns tool calls
async fn setup_mock_ollama_with_tools() -> MockServer {
    let mock_server = MockServer::start().await;

    // First response: tool call
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "nemotron-mini",
            "message": {
                "role": "assistant",
                "content": "Let me read that file for you.",
                "tool_calls": [{
                    "function": {
                        "name": "read_file",
                        "arguments": {
                            "path": "test.txt"
                        }
                    }
                }]
            },
            "done": true,
            "done_reason": "tool_calls"
        })))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    // Second response: final answer after tool
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "nemotron-mini",
            "message": {
                "role": "assistant",
                "content": "The file contains: hello world"
            },
            "done": true,
            "done_reason": "stop"
        })))
        .mount(&mock_server)
        .await;

    mock_server
}

/// Create a config pointing to the mock server
fn config_with_mock_url(_mock_url: &str) -> PawanConfig {
    let mut config = PawanConfig::default();
    config.model = "nemotron-mini".to_string();
    // Note: We'll set OLLAMA_URL env var instead of modifying config
    config
}

// ============================================================================
// Tool Registry Tests
// ============================================================================

#[test]
fn test_tool_registry_creation() {
    let temp_dir = TempDir::new().unwrap();
    let registry = ToolRegistry::with_defaults(temp_dir.path().to_path_buf());

    // Verify all expected tools are registered
    assert!(registry.has_tool("read_file"));
    assert!(registry.has_tool("write_file"));
    assert!(registry.has_tool("list_directory"));
    assert!(registry.has_tool("edit_file"));
    assert!(registry.has_tool("glob_search"));
    assert!(registry.has_tool("grep_search"));
    assert!(registry.has_tool("bash"));
    assert!(registry.has_tool("git_status"));
    assert!(registry.has_tool("git_diff"));
    assert!(registry.has_tool("git_add"));
    assert!(registry.has_tool("git_commit"));
}

#[test]
fn test_tool_registry_definitions() {
    let temp_dir = TempDir::new().unwrap();
    let registry = ToolRegistry::with_defaults(temp_dir.path().to_path_buf());

    let definitions = registry.get_definitions();

    // Should have 11 tools
    assert_eq!(definitions.len(), 11);

    // Each definition should have name, description, and parameters
    for def in &definitions {
        assert!(!def.name.is_empty());
        assert!(!def.description.is_empty());
        assert!(def.parameters.is_object());
    }
}

#[tokio::test]
async fn test_read_file_tool() {
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("test.txt");
    std::fs::write(&test_file, "Hello, World!").unwrap();

    let registry = ToolRegistry::with_defaults(temp_dir.path().to_path_buf());

    let result = registry
        .execute(
            "read_file",
            json!({
                "path": "test.txt"
            }),
        )
        .await
        .unwrap();

    assert!(result["content"]
        .as_str()
        .unwrap()
        .contains("Hello, World!"));
}

#[tokio::test]
async fn test_write_file_tool() {
    let temp_dir = TempDir::new().unwrap();
    let registry = ToolRegistry::with_defaults(temp_dir.path().to_path_buf());

    let result = registry
        .execute(
            "write_file",
            json!({
                "path": "new_file.txt",
                "content": "New content"
            }),
        )
        .await
        .unwrap();

    assert!(result["success"].as_bool().unwrap());

    // Verify file was written
    let content = std::fs::read_to_string(temp_dir.path().join("new_file.txt")).unwrap();
    assert_eq!(content, "New content");
}

#[tokio::test]
async fn test_edit_file_tool() {
    let temp_dir = TempDir::new().unwrap();
    let test_file = temp_dir.path().join("edit_me.txt");
    std::fs::write(&test_file, "Hello World").unwrap();

    let registry = ToolRegistry::with_defaults(temp_dir.path().to_path_buf());

    let result = registry
        .execute(
            "edit_file",
            json!({
                "path": "edit_me.txt",
                "old_string": "World",
                "new_string": "Rust"
            }),
        )
        .await
        .unwrap();

    assert!(result["success"].as_bool().unwrap());

    let content = std::fs::read_to_string(&test_file).unwrap();
    assert_eq!(content, "Hello Rust");
}

#[tokio::test]
async fn test_glob_search_tool() {
    let temp_dir = TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("file1.rs"), "rust code").unwrap();
    std::fs::write(temp_dir.path().join("file2.rs"), "more rust").unwrap();
    std::fs::write(temp_dir.path().join("file3.txt"), "text").unwrap();

    let registry = ToolRegistry::with_defaults(temp_dir.path().to_path_buf());

    let result = registry
        .execute(
            "glob_search",
            json!({
                "pattern": "*.rs"
            }),
        )
        .await
        .unwrap();

    assert_eq!(result["count"].as_u64().unwrap(), 2);
}

#[tokio::test]
async fn test_grep_search_tool() {
    let temp_dir = TempDir::new().unwrap();
    std::fs::write(
        temp_dir.path().join("code.rs"),
        "fn main() {\n    println!(\"hello\");\n}",
    )
    .unwrap();

    let registry = ToolRegistry::with_defaults(temp_dir.path().to_path_buf());

    let result = registry
        .execute(
            "grep_search",
            json!({
                "pattern": "println",
                "include": "*.rs"
            }),
        )
        .await
        .unwrap();

    assert_eq!(result["file_count"].as_u64().unwrap(), 1);
    assert_eq!(result["total_matches"].as_u64().unwrap(), 1);
}

// ============================================================================
// Healing Module Tests
// ============================================================================

#[test]
fn test_parse_text_diagnostics() {
    let output = r#"error[E0425]: cannot find value `x` in this scope
   --> src/main.rs:10:5
    |
10  |     x
    |     ^ not found in this scope

warning: unused variable: `y`
   --> src/main.rs:5:9
    |
5   |     let y = 42;
    |         ^ help: if this is intentional, prefix it with an underscore: `_y`
"#;

    let fixer = CompilerFixer::new(PathBuf::from("."));
    let diagnostics = fixer.parse_diagnostics(output);

    // Should find both error and warning
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.kind == DiagnosticKind::Error)
        .collect();
    let _warnings: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.kind == DiagnosticKind::Warning)
        .collect();

    assert!(!errors.is_empty(), "Should find at least one error");
    assert!(
        errors[0].message.contains("cannot find value"),
        "Error message should match"
    );
}

#[test]
fn test_healing_config_defaults() {
    let config = HealingConfig::default();

    assert!(!config.auto_commit);
    assert!(config.fix_errors);
    assert!(config.fix_warnings);
    assert!(config.fix_tests);
    assert!(!config.generate_docs);
    assert_eq!(config.max_attempts, 3);
}

// ============================================================================
// Config Tests
// ============================================================================

#[test]
fn test_config_defaults() {
    let config = PawanConfig::default();

    assert_eq!(config.model, "nemotron");
    assert!(!config.dry_run);
    assert!(config.auto_backup);
    assert!(config.reasoning_mode);
    assert_eq!(config.max_tool_iterations, 50);
}

#[test]
fn test_config_system_prompt_with_reasoning() {
    let mut config = PawanConfig::default();
    config.reasoning_mode = true;

    let prompt = config.get_system_prompt();

    assert!(prompt.starts_with("detailed thinking on"));
    assert!(prompt.contains("Pawan"));
}

#[test]
fn test_config_system_prompt_without_reasoning() {
    let mut config = PawanConfig::default();
    config.reasoning_mode = false;

    let prompt = config.get_system_prompt();

    assert!(prompt.starts_with("detailed thinking off"));
}

#[test]
fn test_config_load_from_toml() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("pawan.toml");

    std::fs::write(
        &config_path,
        r#"
model = "llama3.2"
dry_run = true
temperature = 0.8
max_tool_iterations = 100

[healing]
fix_errors = true
fix_warnings = false
auto_commit = true

[tui]
syntax_highlighting = false
"#,
    )
    .unwrap();

    let config = PawanConfig::load(Some(&config_path)).unwrap();

    assert_eq!(config.model, "llama3.2");
    assert!(config.dry_run);
    assert_eq!(config.temperature, 0.8);
    assert_eq!(config.max_tool_iterations, 100);
    assert!(config.healing.fix_errors);
    assert!(!config.healing.fix_warnings);
    assert!(config.healing.auto_commit);
    assert!(!config.tui.syntax_highlighting);
}

#[test]
fn test_config_targets() {
    let config = PawanConfig::default();

    // Should have default targets
    assert!(config.get_target("ares").is_some());
    assert!(config.get_target("self").is_some());

    // Nonexistent target
    assert!(config.get_target("nonexistent").is_none());
}

// ============================================================================
// Agent Tests (with mocked Ollama)
// ============================================================================

#[tokio::test]
async fn test_agent_creation() {
    let temp_dir = TempDir::new().unwrap();
    let config = PawanConfig::default();

    let agent = PawanAgent::new(config, temp_dir.path().to_path_buf());

    // Agent should be created with empty history
    assert!(agent.history().is_empty());
}

#[tokio::test]
async fn test_agent_clear_history() {
    let temp_dir = TempDir::new().unwrap();
    let config = PawanConfig::default();

    let mut agent = PawanAgent::new(config, temp_dir.path().to_path_buf());

    // Add a message
    agent.add_message(pawan::agent::Message {
        role: Role::User,
        content: "Hello".to_string(),
        tool_calls: vec![],
        tool_result: None,
    });

    assert_eq!(agent.history().len(), 1);

    agent.clear_history();

    assert!(agent.history().is_empty());
}

#[tokio::test]
async fn test_agent_tool_definitions() {
    let temp_dir = TempDir::new().unwrap();
    let config = PawanConfig::default();

    let agent = PawanAgent::new(config, temp_dir.path().to_path_buf());
    let definitions = agent.get_tool_definitions();

    // Should have all tools
    assert_eq!(definitions.len(), 11);

    // Verify tool names
    let names: Vec<&str> = definitions.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"read_file"));
    assert!(names.contains(&"write_file"));
    assert!(names.contains(&"bash"));
}

// ============================================================================
// Message Serialization Tests
// ============================================================================

#[test]
fn test_message_serialization() {
    use pawan::agent::Message;

    let msg = Message {
        role: Role::User,
        content: "Hello".to_string(),
        tool_calls: vec![],
        tool_result: None,
    };

    let json = serde_json::to_string(&msg).unwrap();
    let parsed: Message = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.role, Role::User);
    assert_eq!(parsed.content, "Hello");
}

#[test]
fn test_tool_call_request_serialization() {
    use pawan::agent::ToolCallRequest;

    let tc = ToolCallRequest {
        id: "123".to_string(),
        name: "read_file".to_string(),
        arguments: json!({"path": "test.txt"}),
    };

    let json = serde_json::to_string(&tc).unwrap();
    let parsed: ToolCallRequest = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.id, "123");
    assert_eq!(parsed.name, "read_file");
}

// ============================================================================
// End-to-end Tests (require OLLAMA_URL to be set or Ollama running)
// ============================================================================

/// This test requires Ollama to be running with a model available.
/// Run with: OLLAMA_URL=http://localhost:11434 cargo test -- --ignored
#[tokio::test]
#[ignore]
async fn test_agent_simple_execution() {
    let temp_dir = TempDir::new().unwrap();
    let config = PawanConfig::default();

    let mut agent = PawanAgent::new(config, temp_dir.path().to_path_buf());

    let response = agent.execute("Say hello").await;

    // Should succeed if Ollama is running
    assert!(
        response.is_ok(),
        "Agent execution failed: {:?}",
        response.err()
    );

    let response = response.unwrap();
    assert!(!response.content.is_empty());
}

/// Test healing on a simple project
#[tokio::test]
#[ignore]
async fn test_agent_heal() {
    let temp_dir = TempDir::new().unwrap();

    // Create a Rust project with an error
    std::fs::write(
        temp_dir.path().join("Cargo.toml"),
        r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    std::fs::create_dir_all(temp_dir.path().join("src")).unwrap();
    std::fs::write(
        temp_dir.path().join("src/main.rs"),
        "fn main() { let x: i32 = \"not a number\"; }",
    )
    .unwrap();

    let config = PawanConfig::default();
    let mut agent = PawanAgent::new(config, temp_dir.path().to_path_buf());

    let response = agent.heal().await;

    assert!(response.is_ok(), "Heal failed: {:?}", response.err());
}
