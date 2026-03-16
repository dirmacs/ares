//! CLI integration tests for Pawan
//!
//! Tests the command-line interface behavior including:
//! - Help and version output
//! - Argument parsing
//! - Subcommand execution
//! - Error handling

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Get the pawan command from cargo
fn pawan_cmd() -> Command {
    Command::cargo_bin("pawan").unwrap()
}

// ============================================================================
// Help & Version Tests
// ============================================================================

#[test]
fn test_help_output() {
    pawan_cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pawan"))
        .stdout(predicate::str::contains("Self-heal"))
        .stdout(predicate::str::contains("heal"))
        .stdout(predicate::str::contains("task"))
        .stdout(predicate::str::contains("commit"))
        .stdout(predicate::str::contains("improve"))
        .stdout(predicate::str::contains("status"));
}

#[test]
fn test_short_help() {
    pawan_cmd()
        .arg("-h")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_version_output() {
    pawan_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("pawan"))
        .stdout(predicate::str::contains("0.1.0"));
}

#[test]
fn test_short_version() {
    pawan_cmd()
        .arg("-V")
        .assert()
        .success()
        .stdout(predicate::str::contains("0.1.0"));
}

// ============================================================================
// Subcommand Help Tests
// ============================================================================

#[test]
fn test_heal_help() {
    pawan_cmd()
        .args(["heal", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Self-heal"))
        .stdout(predicate::str::contains("--errors-only"))
        .stdout(predicate::str::contains("--warnings-only"))
        .stdout(predicate::str::contains("--tests-only"))
        .stdout(predicate::str::contains("--commit"));
}

#[test]
fn test_task_help() {
    pawan_cmd()
        .args(["task", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Execute a coding task"))
        .stdout(predicate::str::contains("<DESCRIPTION>"));
}

#[test]
fn test_commit_help() {
    pawan_cmd()
        .args(["commit", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("commit message"))
        .stdout(predicate::str::contains("--with-body"));
}

#[test]
fn test_improve_help() {
    pawan_cmd()
        .args(["improve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Improve"))
        .stdout(predicate::str::contains("<TARGET>"));
}

#[test]
fn test_status_help() {
    pawan_cmd()
        .args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status"));
}

// ============================================================================
// Global Options Tests
// ============================================================================

#[test]
fn test_workspace_option() {
    let temp_dir = TempDir::new().unwrap();

    pawan_cmd()
        .arg("--workspace")
        .arg(temp_dir.path())
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn test_verbose_option() {
    pawan_cmd()
        .arg("--verbose")
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn test_dry_run_option() {
    pawan_cmd()
        .arg("--dry-run")
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn test_model_option() {
    pawan_cmd()
        .arg("--model")
        .arg("llama3.2")
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn test_no_tui_option() {
    pawan_cmd().arg("--no-tui").arg("--help").assert().success();
}

// ============================================================================
// Status Command Tests
// ============================================================================

#[test]
fn test_status_command_runs() {
    let temp_dir = TempDir::new().unwrap();

    // Create a minimal Rust project
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    fs::create_dir_all(temp_dir.path().join("src")).unwrap();
    fs::write(
        temp_dir.path().join("src/main.rs"),
        "fn main() { println!(\"Hello\"); }",
    )
    .unwrap();

    // Run status command
    pawan_cmd()
        .arg("--workspace")
        .arg(temp_dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Project Status"));
}

#[test]
fn test_status_shows_healthy_project() {
    let temp_dir = TempDir::new().unwrap();

    // Create a valid Rust project
    fs::write(
        temp_dir.path().join("Cargo.toml"),
        r#"[package]
name = "healthy-project"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    fs::create_dir_all(temp_dir.path().join("src")).unwrap();
    fs::write(
        temp_dir.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }",
    )
    .unwrap();

    pawan_cmd()
        .arg("--workspace")
        .arg(temp_dir.path())
        .arg("status")
        .assert()
        .success()
        // Should show summary section
        .stdout(predicate::str::contains("Summary"));
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_invalid_subcommand() {
    pawan_cmd()
        .arg("invalid-command")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn test_task_requires_description() {
    pawan_cmd()
        .arg("task")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_improve_requires_target() {
    pawan_cmd()
        .arg("improve")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_invalid_workspace_path() {
    pawan_cmd()
        .arg("--workspace")
        .arg("/nonexistent/path/that/does/not/exist")
        .arg("status")
        .assert()
        .failure();
}

// ============================================================================
// Config File Tests
// ============================================================================

#[test]
fn test_config_option_accepts_path() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("pawan.toml");

    fs::write(
        &config_path,
        r#"
model = "llama3.2"
dry_run = true
temperature = 0.7
"#,
    )
    .unwrap();

    pawan_cmd()
        .arg("--config")
        .arg(&config_path)
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn test_nonexistent_config_file() {
    pawan_cmd()
        .arg("--config")
        .arg("/nonexistent/pawan.toml")
        .arg("status")
        .assert()
        .failure();
}

// ============================================================================
// Heal Command Argument Tests
// ============================================================================

#[test]
fn test_heal_errors_only_flag() {
    pawan_cmd()
        .args(["heal", "--errors-only", "--help"])
        .assert()
        .success();
}

#[test]
fn test_heal_warnings_only_flag() {
    pawan_cmd()
        .args(["heal", "--warnings-only", "--help"])
        .assert()
        .success();
}

#[test]
fn test_heal_tests_only_flag() {
    pawan_cmd()
        .args(["heal", "--tests-only", "--help"])
        .assert()
        .success();
}

#[test]
fn test_heal_commit_flag() {
    pawan_cmd()
        .args(["heal", "--commit", "--help"])
        .assert()
        .success();
}

// ============================================================================
// Improve Command Argument Tests
// ============================================================================

#[test]
fn test_improve_with_file_option() {
    let temp_dir = TempDir::new().unwrap();

    pawan_cmd()
        .arg("--workspace")
        .arg(temp_dir.path())
        .args(["improve", "docs", "--file", "src/lib.rs", "--help"])
        .assert()
        .success();
}

#[test]
fn test_improve_docs_target() {
    pawan_cmd()
        .args(["improve", "docs", "--help"])
        .assert()
        .success();
}

#[test]
fn test_improve_refactor_target() {
    pawan_cmd()
        .args(["improve", "refactor", "--help"])
        .assert()
        .success();
}

#[test]
fn test_improve_tests_target() {
    pawan_cmd()
        .args(["improve", "tests", "--help"])
        .assert()
        .success();
}
