//! Git operation tools
//!
//! Tools for git status, diff, add, and commit operations.
//! Uses direct command execution for reliability.

use super::Tool;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Tool for checking git status
pub struct GitStatusTool {
    workspace_root: PathBuf,
}

impl GitStatusTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    async fn run_git(&self, args: &[&str]) -> crate::Result<(bool, String, String)> {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

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

        Ok((status.success(), stdout, stderr))
    }
}

#[async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }

    fn description(&self) -> &str {
        "Get the current git status showing staged, unstaged, and untracked files."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "short": {
                    "type": "boolean",
                    "description": "Use short format output (default: false)"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value) -> crate::Result<Value> {
        let short = args["short"].as_bool().unwrap_or(false);

        let mut git_args = vec!["status"];
        if short {
            git_args.push("-s");
        }

        let (success, stdout, stderr) = self.run_git(&git_args).await?;

        if !success {
            return Err(crate::PawanError::Git(format!(
                "git status failed: {}",
                stderr
            )));
        }

        // Also get branch info
        let (_, branch_output, _) = self.run_git(&["branch", "--show-current"]).await?;
        let branch = branch_output.trim().to_string();

        // Check if repo is clean
        let (_, porcelain, _) = self.run_git(&["status", "--porcelain"]).await?;
        let is_clean = porcelain.trim().is_empty();

        Ok(json!({
            "status": stdout.trim(),
            "branch": branch,
            "is_clean": is_clean,
            "success": true
        }))
    }
}

/// Tool for getting git diff
pub struct GitDiffTool {
    workspace_root: PathBuf,
}

impl GitDiffTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    async fn run_git(&self, args: &[&str]) -> crate::Result<(bool, String, String)> {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

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

        Ok((status.success(), stdout, stderr))
    }
}

#[async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }

    fn description(&self) -> &str {
        "Show git diff for staged or unstaged changes. Can diff against a specific commit or branch."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "staged": {
                    "type": "boolean",
                    "description": "Show staged changes only (--cached). Default: false (shows unstaged)"
                },
                "file": {
                    "type": "string",
                    "description": "Specific file to diff (optional)"
                },
                "base": {
                    "type": "string",
                    "description": "Base commit/branch to diff against (e.g., 'main', 'HEAD~3')"
                },
                "stat": {
                    "type": "boolean",
                    "description": "Show diffstat summary instead of full diff"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value) -> crate::Result<Value> {
        let staged = args["staged"].as_bool().unwrap_or(false);
        let file = args["file"].as_str();
        let base = args["base"].as_str();
        let stat = args["stat"].as_bool().unwrap_or(false);

        let mut git_args = vec!["diff"];

        if staged {
            git_args.push("--cached");
        }

        if stat {
            git_args.push("--stat");
        }

        if let Some(b) = base {
            git_args.push(b);
        }

        if let Some(f) = file {
            git_args.push("--");
            git_args.push(f);
        }

        let (success, stdout, stderr) = self.run_git(&git_args).await?;

        if !success {
            return Err(crate::PawanError::Git(format!(
                "git diff failed: {}",
                stderr
            )));
        }

        // Truncate if too large
        let max_size = 100_000;
        let truncated = stdout.len() > max_size;
        let diff = if truncated {
            format!(
                "{}...\n[truncated, {} bytes total]",
                &stdout[..max_size],
                stdout.len()
            )
        } else {
            stdout
        };

        Ok(json!({
            "diff": diff,
            "truncated": truncated,
            "has_changes": !diff.trim().is_empty(),
            "success": true
        }))
    }
}

/// Tool for staging files
pub struct GitAddTool {
    workspace_root: PathBuf,
}

impl GitAddTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    async fn run_git(&self, args: &[&str]) -> crate::Result<(bool, String, String)> {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

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

        Ok((status.success(), stdout, stderr))
    }
}

#[async_trait]
impl Tool for GitAddTool {
    fn name(&self) -> &str {
        "git_add"
    }

    fn description(&self) -> &str {
        "Stage files for commit. Can stage specific files or all changes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "List of files to stage. Use [\".\"] to stage all changes."
                },
                "all": {
                    "type": "boolean",
                    "description": "Stage all changes including untracked files (-A)"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value) -> crate::Result<Value> {
        let all = args["all"].as_bool().unwrap_or(false);
        let files: Vec<&str> = args["files"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let mut git_args = vec!["add"];

        if all {
            git_args.push("-A");
        } else if files.is_empty() {
            return Err(crate::PawanError::Tool(
                "Either 'files' or 'all: true' must be specified".into(),
            ));
        } else {
            for f in &files {
                git_args.push(f);
            }
        }

        let (success, _, stderr) = self.run_git(&git_args).await?;

        if !success {
            return Err(crate::PawanError::Git(format!(
                "git add failed: {}",
                stderr
            )));
        }

        // Get status after adding
        let (_, status_output, _) = self.run_git(&["status", "-s"]).await?;
        let staged_count = status_output
            .lines()
            .filter(|l| l.starts_with('A') || l.starts_with('M') || l.starts_with('D'))
            .count();

        Ok(json!({
            "success": true,
            "staged_count": staged_count,
            "message": if all {
                "Staged all changes".to_string()
            } else {
                format!("Staged {} file(s)", files.len())
            }
        }))
    }
}

/// Tool for creating commits
pub struct GitCommitTool {
    workspace_root: PathBuf,
}

impl GitCommitTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    async fn run_git(&self, args: &[&str]) -> crate::Result<(bool, String, String)> {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

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

        Ok((status.success(), stdout, stderr))
    }
}

#[async_trait]
impl Tool for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }

    fn description(&self) -> &str {
        "Create a git commit with the staged changes. Requires a commit message."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Commit message (required)"
                },
                "body": {
                    "type": "string",
                    "description": "Extended commit body (optional)"
                }
            },
            "required": ["message"]
        })
    }

    async fn execute(&self, args: Value) -> crate::Result<Value> {
        let message = args["message"]
            .as_str()
            .ok_or_else(|| crate::PawanError::Tool("commit message is required".into()))?;

        let body = args["body"].as_str();

        // Check if there are staged changes
        let (_, staged, _) = self.run_git(&["diff", "--cached", "--stat"]).await?;
        if staged.trim().is_empty() {
            return Err(crate::PawanError::Git(
                "No staged changes to commit. Use git_add first.".into(),
            ));
        }

        // Build commit message
        let full_message = if let Some(b) = body {
            format!("{}\n\n{}", message, b)
        } else {
            message.to_string()
        };

        let (success, stdout, stderr) = self.run_git(&["commit", "-m", &full_message]).await?;

        if !success {
            return Err(crate::PawanError::Git(format!(
                "git commit failed: {}",
                stderr
            )));
        }

        // Get the commit hash
        let (_, hash_output, _) = self.run_git(&["rev-parse", "--short", "HEAD"]).await?;
        let commit_hash = hash_output.trim().to_string();

        Ok(json!({
            "success": true,
            "commit_hash": commit_hash,
            "message": message,
            "output": stdout.trim()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn setup_git_repo() -> TempDir {
        let temp_dir = TempDir::new().unwrap();

        // Initialize git repo
        let mut cmd = Command::new("git");
        cmd.args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .await
            .unwrap();

        // Configure git for test
        let mut cmd = Command::new("git");
        cmd.args(["config", "user.email", "test@test.com"])
            .current_dir(temp_dir.path())
            .output()
            .await
            .unwrap();

        let mut cmd = Command::new("git");
        cmd.args(["config", "user.name", "Test User"])
            .current_dir(temp_dir.path())
            .output()
            .await
            .unwrap();

        temp_dir
    }

    #[tokio::test]
    async fn test_git_status_empty_repo() {
        let temp_dir = setup_git_repo().await;

        let tool = GitStatusTool::new(temp_dir.path().to_path_buf());
        let result = tool.execute(json!({})).await.unwrap();

        assert!(result["success"].as_bool().unwrap());
        assert!(result["is_clean"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_git_status_with_untracked() {
        let temp_dir = setup_git_repo().await;

        // Create an untracked file
        std::fs::write(temp_dir.path().join("test.txt"), "hello").unwrap();

        let tool = GitStatusTool::new(temp_dir.path().to_path_buf());
        let result = tool.execute(json!({})).await.unwrap();

        assert!(result["success"].as_bool().unwrap());
        assert!(!result["is_clean"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_git_add_and_commit() {
        let temp_dir = setup_git_repo().await;

        // Create a file
        std::fs::write(temp_dir.path().join("test.txt"), "hello").unwrap();

        // Add the file
        let add_tool = GitAddTool::new(temp_dir.path().to_path_buf());
        let add_result = add_tool
            .execute(json!({
                "files": ["test.txt"]
            }))
            .await
            .unwrap();
        assert!(add_result["success"].as_bool().unwrap());

        // Commit
        let commit_tool = GitCommitTool::new(temp_dir.path().to_path_buf());
        let commit_result = commit_tool
            .execute(json!({
                "message": "Add test file"
            }))
            .await
            .unwrap();
        assert!(commit_result["success"].as_bool().unwrap());
        assert!(!commit_result["commit_hash"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_git_diff_no_changes() {
        let temp_dir = setup_git_repo().await;

        let tool = GitDiffTool::new(temp_dir.path().to_path_buf());
        let result = tool.execute(json!({})).await.unwrap();

        assert!(result["success"].as_bool().unwrap());
        assert!(!result["has_changes"].as_bool().unwrap());
    }
}
