//! Bash command execution tool

use super::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Tool for executing bash commands
pub struct BashTool {
    workspace_root: PathBuf,
}

impl BashTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command. Use for git, cargo, and other shell operations. \
         Commands run in the workspace root directory."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory (optional, defaults to workspace root)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 120)"
                },
                "description": {
                    "type": "string",
                    "description": "Brief description of what this command does"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value) -> crate::Result<Value> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| crate::PawanError::Tool("command is required".into()))?;

        let workdir = args["workdir"]
            .as_str()
            .map(|p| self.workspace_root.join(p))
            .unwrap_or_else(|| self.workspace_root.clone());

        let timeout_secs = args["timeout_secs"]
            .as_u64()
            .unwrap_or(crate::DEFAULT_BASH_TIMEOUT);
        let description = args["description"].as_str().unwrap_or("");

        // Validate workdir exists
        if !workdir.exists() {
            return Err(crate::PawanError::NotFound(format!(
                "Working directory not found: {}",
                workdir.display()
            )));
        }

        // Build command
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&workdir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        // Execute with timeout
        let result = timeout(Duration::from_secs(timeout_secs), async {
            let mut child = cmd.spawn().map_err(crate::PawanError::Io)?;

            let mut stdout = String::new();
            let mut stderr = String::new();

            if let Some(mut stdout_handle) = child.stdout.take() {
                stdout_handle.read_to_string(&mut stdout).await.ok();
            }

            if let Some(mut stderr_handle) = child.stderr.take() {
                stderr_handle.read_to_string(&mut stderr).await.ok();
            }

            let status = child.wait().await.map_err(crate::PawanError::Io)?;

            Ok::<_, crate::PawanError>((status, stdout, stderr))
        })
        .await;

        match result {
            Ok(Ok((status, stdout, stderr))) => {
                // Truncate output if too long
                let max_output = 50000;
                let stdout_truncated = stdout.len() > max_output;
                let stderr_truncated = stderr.len() > max_output;

                let stdout_display = if stdout_truncated {
                    format!(
                        "{}...[truncated, {} bytes total]",
                        &stdout[..max_output],
                        stdout.len()
                    )
                } else {
                    stdout
                };

                let stderr_display = if stderr_truncated {
                    format!(
                        "{}...[truncated, {} bytes total]",
                        &stderr[..max_output],
                        stderr.len()
                    )
                } else {
                    stderr
                };

                Ok(json!({
                    "success": status.success(),
                    "exit_code": status.code().unwrap_or(-1),
                    "stdout": stdout_display,
                    "stderr": stderr_display,
                    "description": description,
                    "command": command
                }))
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(crate::PawanError::Timeout(format!(
                "Command timed out after {} seconds: {}",
                timeout_secs, command
            ))),
        }
    }
}

/// Helper struct for commonly used cargo commands
pub struct CargoCommands;

impl CargoCommands {
    /// Build the project
    pub fn build() -> Value {
        json!({
            "command": "cargo build 2>&1",
            "description": "Build the project"
        })
    }

    /// Build with all features
    pub fn build_all_features() -> Value {
        json!({
            "command": "cargo build --all-features 2>&1",
            "description": "Build with all features enabled"
        })
    }

    /// Run tests
    pub fn test() -> Value {
        json!({
            "command": "cargo test 2>&1",
            "description": "Run all tests"
        })
    }

    /// Run a specific test
    pub fn test_name(name: &str) -> Value {
        json!({
            "command": format!("cargo test {} 2>&1", name),
            "description": format!("Run test: {}", name)
        })
    }

    /// Run clippy
    pub fn clippy() -> Value {
        json!({
            "command": "cargo clippy 2>&1",
            "description": "Run clippy linter"
        })
    }

    /// Run rustfmt check
    pub fn fmt_check() -> Value {
        json!({
            "command": "cargo fmt --check 2>&1",
            "description": "Check code formatting"
        })
    }

    /// Run rustfmt
    pub fn fmt() -> Value {
        json!({
            "command": "cargo fmt 2>&1",
            "description": "Format code"
        })
    }

    /// Check compilation
    pub fn check() -> Value {
        json!({
            "command": "cargo check 2>&1",
            "description": "Check compilation without building"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_bash_echo() {
        let temp_dir = TempDir::new().unwrap();

        let tool = BashTool::new(temp_dir.path().to_path_buf());
        let result = tool
            .execute(json!({
                "command": "echo 'hello world'"
            }))
            .await
            .unwrap();

        assert!(result["success"].as_bool().unwrap());
        assert!(result["stdout"].as_str().unwrap().contains("hello world"));
    }

    #[tokio::test]
    async fn test_bash_failing_command() {
        let temp_dir = TempDir::new().unwrap();

        let tool = BashTool::new(temp_dir.path().to_path_buf());
        let result = tool
            .execute(json!({
                "command": "exit 1"
            }))
            .await
            .unwrap();

        assert!(!result["success"].as_bool().unwrap());
        assert_eq!(result["exit_code"], 1);
    }

    #[tokio::test]
    async fn test_bash_timeout() {
        let temp_dir = TempDir::new().unwrap();

        let tool = BashTool::new(temp_dir.path().to_path_buf());
        let result = tool
            .execute(json!({
                "command": "sleep 10",
                "timeout_secs": 1
            }))
            .await;

        assert!(result.is_err());
        match result {
            Err(crate::PawanError::Timeout(_)) => {}
            _ => panic!("Expected timeout error"),
        }
    }
}
