//! CLI Integration Tests for A.R.E.S
//!
//! Tests the command-line interface functionality including the init command,
//! config command, and agent commands.

use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Helper to get the path to the built binary
fn get_binary_path() -> String {
    // In tests, we use cargo run
    "cargo".to_string()
}

/// Helper to run ares-server with arguments
fn run_ares(args: &[&str], working_dir: Option<&str>) -> std::process::Output {
    let mut cmd = Command::new(get_binary_path());
    cmd.arg("run").arg("--quiet").arg("--").args(args);

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    cmd.output().expect("Failed to execute command")
}

// =============================================================================
// Help and Version Tests
// =============================================================================

#[test]
fn test_help_command() {
    let output = run_ares(&["--help"], None);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check key sections are present
    assert!(stdout.contains("A.R.E.S"));
    assert!(stdout.contains("USAGE") || stdout.contains("Usage"));
    assert!(stdout.contains("init"));
    assert!(stdout.contains("config"));
    assert!(stdout.contains("agent"));
}

#[test]
fn test_version_command() {
    let output = run_ares(&["--version"], None);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ares-server"));
}

#[test]
fn test_init_help() {
    let output = run_ares(&["init", "--help"], None);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check init-specific options
    assert!(stdout.contains("--force"));
    assert!(stdout.contains("--minimal"));
    assert!(stdout.contains("--provider"));
    assert!(stdout.contains("--host"));
    assert!(stdout.contains("--port"));
    assert!(stdout.contains("--no-examples"));
}

// =============================================================================
// Init Command Tests
// =============================================================================

#[test]
fn test_init_creates_ares_toml() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    let output = run_ares(&["init", temp_path], None);

    assert!(output.status.success(), "Init command failed: {:?}", output);

    // Check ares.toml was created
    let config_path = temp_dir.path().join("ares.toml");
    assert!(config_path.exists(), "ares.toml was not created");

    // Verify content
    let content = fs::read_to_string(&config_path).expect("Failed to read ares.toml");
    assert!(content.contains("[server]"));
    assert!(content.contains("[auth]"));
    assert!(content.contains("[database]"));
    assert!(content.contains("[providers.ollama-local]"));
}

#[test]
fn test_init_creates_directories() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    let output = run_ares(&["init", temp_path], None);
    assert!(output.status.success());

    // Check directories were created
    assert!(temp_dir.path().join("data").is_dir());
    assert!(temp_dir.path().join("config").is_dir());
    assert!(temp_dir.path().join("config/agents").is_dir());
    assert!(temp_dir.path().join("config/models").is_dir());
    assert!(temp_dir.path().join("config/tools").is_dir());
    assert!(temp_dir.path().join("config/workflows").is_dir());
    assert!(temp_dir.path().join("config/mcps").is_dir());
}

#[test]
fn test_init_creates_env_example() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    let output = run_ares(&["init", temp_path], None);
    assert!(output.status.success());

    let env_path = temp_dir.path().join(".env.example");
    assert!(env_path.exists(), ".env.example was not created");

    let content = fs::read_to_string(&env_path).expect("Failed to read .env.example");
    assert!(content.contains("JWT_SECRET"));
    assert!(content.contains("API_KEY"));
}

#[test]
fn test_init_creates_gitignore() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    let output = run_ares(&["init", temp_path], None);
    assert!(output.status.success());

    let gitignore_path = temp_dir.path().join(".gitignore");
    assert!(gitignore_path.exists(), ".gitignore was not created");

    let content = fs::read_to_string(&gitignore_path).expect("Failed to read .gitignore");
    assert!(content.contains("/data/"));
    assert!(content.contains(".env"));
}

#[test]
fn test_init_creates_toon_files() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    let output = run_ares(&["init", temp_path], None);
    assert!(output.status.success());

    // Check model TOON files
    assert!(temp_dir.path().join("config/models/fast.toon").exists());
    assert!(temp_dir.path().join("config/models/balanced.toon").exists());
    assert!(temp_dir.path().join("config/models/powerful.toon").exists());

    // Check agent TOON files
    assert!(temp_dir.path().join("config/agents/router.toon").exists());
    assert!(temp_dir
        .path()
        .join("config/agents/orchestrator.toon")
        .exists());

    // Check tool TOON files
    assert!(temp_dir
        .path()
        .join("config/tools/calculator.toon")
        .exists());
    assert!(temp_dir
        .path()
        .join("config/tools/web_search.toon")
        .exists());

    // Check workflow TOON files
    assert!(temp_dir
        .path()
        .join("config/workflows/default.toon")
        .exists());
    assert!(temp_dir
        .path()
        .join("config/workflows/research.toon")
        .exists());
}

#[test]
fn test_init_with_openai_provider() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    let output = run_ares(&["init", temp_path, "--provider", "openai"], None);
    assert!(output.status.success());

    let content =
        fs::read_to_string(temp_dir.path().join("ares.toml")).expect("Failed to read ares.toml");
    assert!(content.contains("[providers.openai]"));
    assert!(content.contains("type = \"openai\""));
    assert!(content.contains("OPENAI_API_KEY"));
}

#[test]
fn test_init_with_both_providers() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    let output = run_ares(&["init", temp_path, "--provider", "both"], None);
    assert!(output.status.success());

    let content =
        fs::read_to_string(temp_dir.path().join("ares.toml")).expect("Failed to read ares.toml");
    assert!(content.contains("[providers.ollama-local]"));
    assert!(content.contains("[providers.openai]"));
}

#[test]
fn test_init_with_custom_host_port() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    let output = run_ares(
        &["init", temp_path, "--host", "0.0.0.0", "--port", "8080"],
        None,
    );
    assert!(output.status.success());

    let content =
        fs::read_to_string(temp_dir.path().join("ares.toml")).expect("Failed to read ares.toml");
    assert!(content.contains("host = \"0.0.0.0\""));
    assert!(content.contains("port = 8080"));
}

#[test]
fn test_init_no_examples_flag() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    let output = run_ares(&["init", temp_path, "--no-examples"], None);
    assert!(output.status.success());

    // ares.toml should still exist
    assert!(temp_dir.path().join("ares.toml").exists());

    // But TOON files should not exist
    assert!(!temp_dir.path().join("config/models/fast.toon").exists());
    assert!(!temp_dir.path().join("config/agents/router.toon").exists());
}

#[test]
fn test_init_fails_without_force_when_exists() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    // First init
    let output1 = run_ares(&["init", temp_path], None);
    assert!(output1.status.success());

    // Second init without --force should fail/warn
    let output2 = run_ares(&["init", temp_path], None);

    // Check that it recognized the existing config
    let stderr = String::from_utf8_lossy(&output2.stderr);
    let stdout = String::from_utf8_lossy(&output2.stdout);
    let combined = format!("{}{}", stdout, stderr);

    assert!(
        combined.contains("already exists") || !output2.status.success(),
        "Should warn about existing config or fail"
    );
}

#[test]
fn test_init_with_force_overwrites() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    // First init with default port
    let output1 = run_ares(&["init", temp_path], None);
    assert!(output1.status.success());

    // Second init with --force and different port
    let output2 = run_ares(&["init", temp_path, "--force", "--port", "9999"], None);
    assert!(output2.status.success());

    // Verify new port is in config
    let content =
        fs::read_to_string(temp_dir.path().join("ares.toml")).expect("Failed to read ares.toml");
    assert!(content.contains("port = 9999"));
}

// =============================================================================
// Config Command Tests
// =============================================================================

#[test]
fn test_config_command_with_valid_config() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    // Initialize first
    let init_output = run_ares(&["init", temp_path], None);
    assert!(init_output.status.success());

    // Run config command
    let config_path = temp_dir.path().join("ares.toml");
    let output = run_ares(&["config", "--config", config_path.to_str().unwrap()], None);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check output contains expected sections
    assert!(stdout.contains("Configuration") || stdout.contains("config"));
    assert!(stdout.contains("127.0.0.1") || stdout.contains("Server"));
}

#[test]
fn test_config_command_missing_file() {
    let output = run_ares(&["config", "--config", "/nonexistent/path/ares.toml"], None);

    // Should fail gracefully
    assert!(
        !output.status.success() || {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            stderr.contains("not found") || stdout.contains("not found")
        }
    );
}

// =============================================================================
// Agent Command Tests
// =============================================================================

#[test]
fn test_agent_list_command() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    // Initialize first
    let init_output = run_ares(&["init", temp_path], None);
    assert!(init_output.status.success());

    // Run agent list command
    let config_path = temp_dir.path().join("ares.toml");
    let output = run_ares(
        &["agent", "list", "--config", config_path.to_str().unwrap()],
        None,
    );

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check agents are listed
    assert!(stdout.contains("router") || stdout.contains("Router"));
    assert!(stdout.contains("orchestrator") || stdout.contains("Orchestrator"));
}

#[test]
fn test_agent_show_command() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    // Initialize first
    let init_output = run_ares(&["init", temp_path], None);
    assert!(init_output.status.success());

    // Run agent show command
    let config_path = temp_dir.path().join("ares.toml");
    let output = run_ares(
        &[
            "agent",
            "show",
            "orchestrator",
            "--config",
            config_path.to_str().unwrap(),
        ],
        None,
    );

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check agent details are shown
    assert!(stdout.contains("orchestrator") || stdout.contains("Orchestrator"));
    assert!(stdout.contains("powerful") || stdout.contains("Model"));
}

#[test]
fn test_agent_show_nonexistent() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    // Initialize first
    let init_output = run_ares(&["init", temp_path], None);
    assert!(init_output.status.success());

    // Run agent show command for nonexistent agent
    let config_path = temp_dir.path().join("ares.toml");
    let output = run_ares(
        &[
            "agent",
            "show",
            "nonexistent_agent",
            "--config",
            config_path.to_str().unwrap(),
        ],
        None,
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    assert!(combined.contains("not found") || combined.contains("Not found"));
}

// =============================================================================
// No-Color Flag Tests
// =============================================================================

#[test]
fn test_no_color_flag() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let temp_path = temp_dir.path().to_str().unwrap();

    let output = run_ares(&["--no-color", "init", temp_path], None);
    assert!(output.status.success());

    // The output should not contain ANSI escape codes
    let stdout = String::from_utf8_lossy(&output.stdout);
    // ANSI escape codes start with \x1b[
    assert!(
        !stdout.contains("\x1b["),
        "Output should not contain ANSI escape codes when --no-color is used"
    );
}

// =============================================================================
// Output Module Unit Tests
// =============================================================================

#[cfg(test)]
mod output_tests {
    use ares::cli::output::Output;

    #[test]
    fn test_output_new() {
        let output = Output::new();
        assert!(output.colored);
    }

    #[test]
    fn test_output_no_color() {
        let output = Output::no_color();
        assert!(!output.colored);
    }

    #[test]
    fn test_output_default() {
        let output = Output::default();
        assert!(output.colored);
    }
}

// =============================================================================
// Init Module Unit Tests
// =============================================================================

#[cfg(test)]
mod init_tests {
    use ares::cli::init::{InitConfig, InitResult};
    use std::path::PathBuf;

    #[test]
    fn test_init_config_creation() {
        let config = InitConfig {
            path: PathBuf::from("/tmp/test"),
            force: false,
            minimal: false,
            no_examples: false,
            provider: "ollama".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3000,
        };

        assert_eq!(config.path, PathBuf::from("/tmp/test"));
        assert!(!config.force);
        assert!(!config.minimal);
        assert_eq!(config.provider, "ollama");
        assert_eq!(config.port, 3000);
    }

    #[test]
    fn test_init_result_variants() {
        // Just verify the enum variants exist
        let _success = InitResult::Success;
        let _exists = InitResult::AlreadyExists;
        let _error = InitResult::Error("test error".to_string());
    }
}
