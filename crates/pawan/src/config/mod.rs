//! Configuration for Pawan
//!
//! Pawan can be configured via:
//! - `pawan.toml` in the current directory
//! - `[pawan]` section in `ares.toml`
//! - Environment variables
//! - Command line arguments

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// LLM Provider type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    /// NVIDIA API (build.nvidia.com) - default
    #[default]
    Nvidia,
    /// Local Ollama instance
    Ollama,
    /// OpenAI-compatible API
    OpenAI,
}

/// Main configuration for Pawan
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PawanConfig {
    /// LLM provider to use
    pub provider: LlmProvider,

    /// LLM model to use for coding tasks
    pub model: String,

    /// Enable dry-run mode (show changes without applying)
    pub dry_run: bool,

    /// Create backups before editing files
    pub auto_backup: bool,

    /// Require clean git working directory
    pub require_git_clean: bool,

    /// Timeout for bash commands (seconds)
    pub bash_timeout_secs: u64,

    /// Maximum file size to read (KB)
    pub max_file_size_kb: usize,

    /// Maximum tool iterations per request
    pub max_tool_iterations: usize,

    /// System prompt override
    pub system_prompt: Option<String>,

    /// Temperature for LLM responses
    pub temperature: f32,

    /// Top-p sampling parameter
    pub top_p: f32,

    /// Maximum tokens in response
    pub max_tokens: usize,

    /// Enable reasoning/thinking mode (for DeepSeek/Nemotron models)
    pub reasoning_mode: bool,

    /// Healing configuration
    pub healing: HealingConfig,

    /// Target projects
    pub targets: HashMap<String, TargetConfig>,

    /// TUI configuration
    pub tui: TuiConfig,
}

impl Default for PawanConfig {
    fn default() -> Self {
        let mut targets = HashMap::new();
        targets.insert(
            "ares".to_string(),
            TargetConfig {
                path: PathBuf::from("../.."),
                description: "A.R.E.S server codebase".to_string(),
            },
        );
        targets.insert(
            "self".to_string(),
            TargetConfig {
                path: PathBuf::from("."),
                description: "Pawan's own codebase".to_string(),
            },
        );

        Self {
            provider: LlmProvider::Nvidia,
            model: crate::DEFAULT_MODEL.to_string(),
            dry_run: false,
            auto_backup: true,
            require_git_clean: false,
            bash_timeout_secs: crate::DEFAULT_BASH_TIMEOUT,
            max_file_size_kb: 1024,
            max_tool_iterations: crate::MAX_TOOL_ITERATIONS,
            system_prompt: None,
            temperature: 1.0,
            top_p: 0.95,
            max_tokens: 8192,
            reasoning_mode: true,
            healing: HealingConfig::default(),
            targets,
            tui: TuiConfig::default(),
        }
    }
}

/// Configuration for self-healing behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HealingConfig {
    /// Automatically commit fixes
    pub auto_commit: bool,

    /// Fix compilation errors
    pub fix_errors: bool,

    /// Fix clippy warnings
    pub fix_warnings: bool,

    /// Fix failing tests
    pub fix_tests: bool,

    /// Generate missing documentation
    pub generate_docs: bool,

    /// Maximum fix attempts per issue
    pub max_attempts: usize,
}

impl Default for HealingConfig {
    fn default() -> Self {
        Self {
            auto_commit: false,
            fix_errors: true,
            fix_warnings: true,
            fix_tests: true,
            generate_docs: false,
            max_attempts: 3,
        }
    }
}

/// Configuration for a target project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetConfig {
    /// Path to the project root
    pub path: PathBuf,

    /// Description of the project
    pub description: String,
}

/// Configuration for the TUI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    /// Enable syntax highlighting
    pub syntax_highlighting: bool,

    /// Theme for syntax highlighting
    pub theme: String,

    /// Show line numbers in code blocks
    pub line_numbers: bool,

    /// Enable mouse support
    pub mouse_support: bool,

    /// Scroll speed (lines per scroll event)
    pub scroll_speed: usize,

    /// Maximum history entries to keep
    pub max_history: usize,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            syntax_highlighting: true,
            theme: "base16-ocean.dark".to_string(),
            line_numbers: true,
            mouse_support: true,
            scroll_speed: 3,
            max_history: 1000,
        }
    }
}

impl PawanConfig {
    /// Load configuration from file
    pub fn load(path: Option<&PathBuf>) -> crate::Result<Self> {
        let config_path = path.cloned().or_else(|| {
            // Try pawan.toml first
            let pawan_toml = PathBuf::from("pawan.toml");
            if pawan_toml.exists() {
                return Some(pawan_toml);
            }

            // Try ares.toml
            let ares_toml = PathBuf::from("ares.toml");
            if ares_toml.exists() {
                return Some(ares_toml);
            }

            None
        });

        match config_path {
            Some(path) => {
                let content = std::fs::read_to_string(&path).map_err(|e| {
                    crate::PawanError::Config(format!("Failed to read {}: {}", path.display(), e))
                })?;

                // Check if this is ares.toml (look for [pawan] section)
                if path.file_name().map(|n| n == "ares.toml").unwrap_or(false) {
                    // Parse as TOML and extract [pawan] section
                    let value: toml::Value = toml::from_str(&content).map_err(|e| {
                        crate::PawanError::Config(format!(
                            "Failed to parse {}: {}",
                            path.display(),
                            e
                        ))
                    })?;

                    if let Some(pawan_section) = value.get("pawan") {
                        let config: PawanConfig =
                            pawan_section.clone().try_into().map_err(|e| {
                                crate::PawanError::Config(format!(
                                    "Failed to parse [pawan] section: {}",
                                    e
                                ))
                            })?;
                        return Ok(config);
                    }

                    // No [pawan] section, use defaults
                    Ok(Self::default())
                } else {
                    // Parse as pawan.toml
                    toml::from_str(&content).map_err(|e| {
                        crate::PawanError::Config(format!(
                            "Failed to parse {}: {}",
                            path.display(),
                            e
                        ))
                    })
                }
            }
            None => Ok(Self::default()),
        }
    }

    /// Get target by name
    pub fn get_target(&self, name: &str) -> Option<&TargetConfig> {
        self.targets.get(name)
    }

    /// Get the system prompt, with reasoning mode prefix for DeepSeek/thinking models
    pub fn get_system_prompt(&self) -> String {
        self.system_prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string())
    }

    /// Check if thinking mode should be enabled (for DeepSeek models)
    pub fn use_thinking_mode(&self) -> bool {
        self.reasoning_mode && self.model.contains("deepseek")
    }
}

/// Default system prompt for coding tasks
pub const DEFAULT_SYSTEM_PROMPT: &str = r#"You are Pawan, an expert coding assistant with deep knowledge of Rust, software engineering best practices, and the ability to read, write, and modify code.

You have access to tools for:
- Reading and writing files
- Executing bash commands
- Searching codebases (glob patterns and content search)
- Git operations (status, diff, add, commit)
- Cargo operations (build, test, clippy, fmt)

When making changes:
1. Always read files before modifying them to understand context
2. Make minimal, focused changes
3. Explain your reasoning before making changes
4. Verify changes compile and tests pass when appropriate
5. Follow existing code style and patterns

When fixing issues:
1. Understand the root cause before attempting fixes
2. Make one fix at a time and verify it works
3. If a fix doesn't work, try a different approach
4. Document what you changed and why

Be concise in explanations but thorough in code changes."#;
