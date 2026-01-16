//! Self-healing module for Pawan
//!
//! This module provides automated fixing capabilities for:
//! - Compilation errors (`rustc` errors)
//! - Clippy warnings
//! - Test failures
//! - Missing documentation

use crate::config::HealingConfig;
use crate::{PawanError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// A compilation diagnostic (error or warning)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The type of diagnostic
    pub kind: DiagnosticKind,
    /// The diagnostic message
    pub message: String,
    /// File path where the issue is
    pub file: Option<PathBuf>,
    /// Line number (1-indexed)
    pub line: Option<usize>,
    /// Column number (1-indexed)  
    pub column: Option<usize>,
    /// The error/warning code (e.g., E0425)
    pub code: Option<String>,
    /// Suggested fix from the compiler
    pub suggestion: Option<String>,
    /// Full raw output for context
    pub raw: String,
}

/// Type of diagnostic
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticKind {
    Error,
    Warning,
    Note,
    Help,
}

/// Result from a healing operation
#[derive(Debug)]
pub struct HealingResult {
    /// Number of issues found
    pub issues_found: usize,
    /// Number of issues fixed
    pub issues_fixed: usize,
    /// Remaining unfixed issues
    pub remaining: Vec<Diagnostic>,
    /// Description of what was done
    pub summary: String,
}

/// Compiler error fixer
pub struct CompilerFixer {
    workspace_root: PathBuf,
}

impl CompilerFixer {
    /// Create a new CompilerFixer
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    /// Parse cargo check output into diagnostics
    pub fn parse_diagnostics(&self, output: &str) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();

        // Parse the JSON output from cargo check --message-format=json
        for line in output.lines() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(msg) = json.get("message") {
                    let diagnostic = self.parse_diagnostic_message(msg, line);
                    if let Some(d) = diagnostic {
                        diagnostics.push(d);
                    }
                }
            }
        }

        // If no JSON output, try parsing text format
        if diagnostics.is_empty() {
            diagnostics = self.parse_text_diagnostics(output);
        }

        diagnostics
    }

    /// Parse a JSON diagnostic message
    fn parse_diagnostic_message(&self, msg: &serde_json::Value, raw: &str) -> Option<Diagnostic> {
        let level = msg.get("level")?.as_str()?;
        let message = msg.get("message")?.as_str()?.to_string();

        let kind = match level {
            "error" => DiagnosticKind::Error,
            "warning" => DiagnosticKind::Warning,
            "note" => DiagnosticKind::Note,
            "help" => DiagnosticKind::Help,
            _ => return None,
        };

        // Skip ICE messages and internal errors
        if message.contains("internal compiler error") {
            return None;
        }

        // Extract code
        let code = msg
            .get("code")
            .and_then(|c| c.get("code"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());

        // Extract primary span
        let spans = msg.get("spans")?.as_array()?;
        let primary_span = spans.iter().find(|s| {
            s.get("is_primary")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        });

        let (file, line, column) = if let Some(span) = primary_span {
            let file = span
                .get("file_name")
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
            let line = span
                .get("line_start")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let column = span
                .get("column_start")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            (file, line, column)
        } else {
            (None, None, None)
        };

        // Extract suggestion
        let suggestion = msg
            .get("children")
            .and_then(|c| c.as_array())
            .and_then(|children| {
                children.iter().find_map(|child| {
                    let level = child.get("level")?.as_str()?;
                    if level == "help" {
                        let help_msg = child.get("message")?.as_str()?;
                        // Look for suggested replacement
                        if let Some(spans) = child.get("spans").and_then(|s| s.as_array()) {
                            for span in spans {
                                if let Some(replacement) =
                                    span.get("suggested_replacement").and_then(|v| v.as_str())
                                {
                                    return Some(format!("{}: {}", help_msg, replacement));
                                }
                            }
                        }
                        return Some(help_msg.to_string());
                    }
                    None
                })
            });

        Some(Diagnostic {
            kind,
            message,
            file,
            line,
            column,
            code,
            suggestion,
            raw: raw.to_string(),
        })
    }

    /// Parse text format diagnostics (fallback)
    fn parse_text_diagnostics(&self, output: &str) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let mut current_diagnostic: Option<Diagnostic> = None;

        for line in output.lines() {
            // Match pattern: error[E0425]: cannot find value `x` in this scope
            if line.starts_with("error") || line.starts_with("warning") {
                // Save previous diagnostic
                if let Some(d) = current_diagnostic.take() {
                    diagnostics.push(d);
                }

                let kind = if line.starts_with("error") {
                    DiagnosticKind::Error
                } else {
                    DiagnosticKind::Warning
                };

                // Extract code like E0425
                let code = line
                    .find('[')
                    .and_then(|start| line.find(']').map(|end| line[start + 1..end].to_string()));

                // Extract message
                let message = if let Some(colon_pos) = line.find("]: ") {
                    line[colon_pos + 3..].to_string()
                } else if let Some(colon_pos) = line.find(": ") {
                    line[colon_pos + 2..].to_string()
                } else {
                    line.to_string()
                };

                current_diagnostic = Some(Diagnostic {
                    kind,
                    message,
                    file: None,
                    line: None,
                    column: None,
                    code,
                    suggestion: None,
                    raw: line.to_string(),
                });
            }
            // Match pattern: --> src/main.rs:10:5
            else if line.trim().starts_with("-->") {
                if let Some(ref mut d) = current_diagnostic {
                    let path_part = line.trim().trim_start_matches("-->").trim();
                    // Parse file:line:column
                    let parts: Vec<&str> = path_part.rsplitn(3, ':').collect();
                    if parts.len() >= 2 {
                        d.column = parts[0].parse().ok();
                        d.line = parts[1].parse().ok();
                        if parts.len() >= 3 {
                            d.file = Some(PathBuf::from(parts[2]));
                        }
                    }
                }
            }
            // Match help suggestions
            else if line.trim().starts_with("help:") {
                if let Some(ref mut d) = current_diagnostic {
                    let suggestion = line.trim().trim_start_matches("help:").trim();
                    d.suggestion = Some(suggestion.to_string());
                }
            }
        }

        // Don't forget the last one
        if let Some(d) = current_diagnostic {
            diagnostics.push(d);
        }

        diagnostics
    }

    /// Run cargo check and get diagnostics
    pub async fn check(&self) -> Result<Vec<Diagnostic>> {
        let output = self.run_cargo(&["check", "--message-format=json"]).await?;
        Ok(self.parse_diagnostics(&output))
    }

    /// Run a cargo command
    async fn run_cargo(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("cargo");
        cmd.args(args)
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        let mut child = cmd.spawn().map_err(PawanError::Io)?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let Some(mut handle) = child.stdout.take() {
            handle.read_to_string(&mut stdout).await.ok();
        }

        if let Some(mut handle) = child.stderr.take() {
            handle.read_to_string(&mut stderr).await.ok();
        }

        child.wait().await.map_err(PawanError::Io)?;

        // Combine stdout and stderr
        Ok(format!("{}\n{}", stdout, stderr))
    }
}

/// Clippy warning fixer
pub struct ClippyFixer {
    workspace_root: PathBuf,
}

impl ClippyFixer {
    /// Create a new ClippyFixer
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    /// Run clippy and get warnings
    pub async fn check(&self) -> Result<Vec<Diagnostic>> {
        let output = self.run_clippy().await?;
        let fixer = CompilerFixer::new(self.workspace_root.clone());
        let mut diagnostics = fixer.parse_diagnostics(&output);

        // Filter to only warnings
        diagnostics.retain(|d| d.kind == DiagnosticKind::Warning);

        Ok(diagnostics)
    }

    /// Run clippy with JSON output
    async fn run_clippy(&self) -> Result<String> {
        let mut cmd = Command::new("cargo");
        cmd.args(["clippy", "--message-format=json", "--", "-W", "clippy::all"])
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        let mut child = cmd.spawn().map_err(PawanError::Io)?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let Some(mut handle) = child.stdout.take() {
            handle.read_to_string(&mut stdout).await.ok();
        }

        if let Some(mut handle) = child.stderr.take() {
            handle.read_to_string(&mut stderr).await.ok();
        }

        child.wait().await.map_err(PawanError::Io)?;

        Ok(format!("{}\n{}", stdout, stderr))
    }
}

/// Test failure fixer
pub struct TestFixer {
    workspace_root: PathBuf,
}

/// A failed test
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedTest {
    /// Test name (full path)
    pub name: String,
    /// Test module path
    pub module: String,
    /// Failure message/output
    pub failure: String,
    /// Location of the test
    pub file: Option<PathBuf>,
    /// Line number
    pub line: Option<usize>,
}

impl TestFixer {
    /// Create a new TestFixer
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    /// Run tests and get failures
    pub async fn check(&self) -> Result<Vec<FailedTest>> {
        let output = self.run_tests().await?;
        Ok(self.parse_test_output(&output))
    }

    /// Run cargo test
    async fn run_tests(&self) -> Result<String> {
        let mut cmd = Command::new("cargo");
        cmd.args(["test", "--no-fail-fast", "--", "--nocapture"])
            .current_dir(&self.workspace_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        let mut child = cmd.spawn().map_err(PawanError::Io)?;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let Some(mut handle) = child.stdout.take() {
            handle.read_to_string(&mut stdout).await.ok();
        }

        if let Some(mut handle) = child.stderr.take() {
            handle.read_to_string(&mut stderr).await.ok();
        }

        child.wait().await.map_err(PawanError::Io)?;

        Ok(format!("{}\n{}", stdout, stderr))
    }

    /// Parse test output for failures
    fn parse_test_output(&self, output: &str) -> Vec<FailedTest> {
        let mut failures = Vec::new();
        let mut in_failures_section = false;
        let mut current_test: Option<String> = None;
        let mut current_output = String::new();

        for line in output.lines() {
            // Detect failures section
            if line.contains("failures:") && !line.contains("test result:") {
                in_failures_section = true;
                continue;
            }

            // End of failures section
            if in_failures_section && line.starts_with("test result:") {
                // Save last failure
                if let Some(test_name) = current_test.take() {
                    failures.push(FailedTest {
                        name: test_name.clone(),
                        module: self.extract_module(&test_name),
                        failure: current_output.trim().to_string(),
                        file: None,
                        line: None,
                    });
                }
                break;
            }

            // Detect individual test failure
            if line.starts_with("---- ") && line.ends_with(" stdout ----") {
                // Save previous failure
                if let Some(test_name) = current_test.take() {
                    failures.push(FailedTest {
                        name: test_name.clone(),
                        module: self.extract_module(&test_name),
                        failure: current_output.trim().to_string(),
                        file: None,
                        line: None,
                    });
                }

                // Start new failure
                let test_name = line
                    .trim_start_matches("---- ")
                    .trim_end_matches(" stdout ----")
                    .to_string();
                current_test = Some(test_name);
                current_output.clear();
            } else if current_test.is_some() {
                current_output.push_str(line);
                current_output.push('\n');
            }
        }

        // Also look for simple FAILED lines
        for line in output.lines() {
            if line.contains("FAILED") && line.starts_with("test ") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let test_name = parts[1].trim_end_matches(" ...");

                    // Check if we already have this failure
                    if !failures.iter().any(|f| f.name == test_name) {
                        failures.push(FailedTest {
                            name: test_name.to_string(),
                            module: self.extract_module(test_name),
                            failure: line.to_string(),
                            file: None,
                            line: None,
                        });
                    }
                }
            }
        }

        failures
    }

    /// Extract module path from test name
    fn extract_module(&self, test_name: &str) -> String {
        if let Some(pos) = test_name.rfind("::") {
            test_name[..pos].to_string()
        } else {
            String::new()
        }
    }
}

/// Healer that coordinates all fixing activities
pub struct Healer {
    #[allow(dead_code)]
    workspace_root: PathBuf,
    config: HealingConfig,
    compiler_fixer: CompilerFixer,
    clippy_fixer: ClippyFixer,
    test_fixer: TestFixer,
}

impl Healer {
    /// Create a new Healer
    pub fn new(workspace_root: PathBuf, config: HealingConfig) -> Self {
        Self {
            compiler_fixer: CompilerFixer::new(workspace_root.clone()),
            clippy_fixer: ClippyFixer::new(workspace_root.clone()),
            test_fixer: TestFixer::new(workspace_root.clone()),
            workspace_root,
            config,
        }
    }

    /// Get all diagnostics (errors and warnings)
    pub async fn get_diagnostics(&self) -> Result<Vec<Diagnostic>> {
        let mut all = Vec::new();

        if self.config.fix_errors {
            all.extend(self.compiler_fixer.check().await?);
        }

        if self.config.fix_warnings {
            all.extend(self.clippy_fixer.check().await?);
        }

        Ok(all)
    }

    /// Get all failed tests
    pub async fn get_failed_tests(&self) -> Result<Vec<FailedTest>> {
        if self.config.fix_tests {
            self.test_fixer.check().await
        } else {
            Ok(Vec::new())
        }
    }

    /// Count total issues
    pub async fn count_issues(&self) -> Result<(usize, usize, usize)> {
        let diagnostics = self.get_diagnostics().await?;
        let tests = self.get_failed_tests().await?;

        let errors = diagnostics
            .iter()
            .filter(|d| d.kind == DiagnosticKind::Error)
            .count();
        let warnings = diagnostics
            .iter()
            .filter(|d| d.kind == DiagnosticKind::Warning)
            .count();
        let failed_tests = tests.len();

        Ok((errors, warnings, failed_tests))
    }

    /// Format diagnostics for LLM prompt
    pub fn format_diagnostics_for_prompt(&self, diagnostics: &[Diagnostic]) -> String {
        let mut output = String::new();

        for (i, d) in diagnostics.iter().enumerate() {
            output.push_str(&format!("\n### Issue {}\n", i + 1));
            output.push_str(&format!("Type: {:?}\n", d.kind));

            if let Some(ref code) = d.code {
                output.push_str(&format!("Code: {}\n", code));
            }

            output.push_str(&format!("Message: {}\n", d.message));

            if let Some(ref file) = d.file {
                output.push_str(&format!(
                    "Location: {}:{}:{}\n",
                    file.display(),
                    d.line.unwrap_or(0),
                    d.column.unwrap_or(0)
                ));
            }

            if let Some(ref suggestion) = d.suggestion {
                output.push_str(&format!("Suggestion: {}\n", suggestion));
            }
        }

        output
    }

    /// Format failed tests for LLM prompt
    pub fn format_tests_for_prompt(&self, tests: &[FailedTest]) -> String {
        let mut output = String::new();

        for (i, test) in tests.iter().enumerate() {
            output.push_str(&format!("\n### Failed Test {}\n", i + 1));
            output.push_str(&format!("Name: {}\n", test.name));
            output.push_str(&format!("Module: {}\n", test.module));
            output.push_str(&format!("Failure:\n```\n{}\n```\n", test.failure));
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_text_diagnostic() {
        let output = r#"error[E0425]: cannot find value `x` in this scope
   --> src/main.rs:10:5
    |
10  |     x
    |     ^ not found in this scope
"#;

        let fixer = CompilerFixer::new(PathBuf::from("."));
        let diagnostics = fixer.parse_text_diagnostics(output);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, DiagnosticKind::Error);
        assert_eq!(diagnostics[0].code, Some("E0425".to_string()));
        assert!(diagnostics[0].message.contains("cannot find value"));
    }

    #[test]
    fn test_extract_module() {
        let fixer = TestFixer::new(PathBuf::from("."));

        assert_eq!(
            fixer.extract_module("crate::module::tests::test_foo"),
            "crate::module::tests"
        );
        assert_eq!(fixer.extract_module("test_foo"), "");
    }
}
