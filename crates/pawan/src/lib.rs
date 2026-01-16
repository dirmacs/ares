//! # Pawan (पवन) - Self-Healing CLI Coding Agent
//!
//! Pawan is a powerful CLI coding agent that integrates with A.R.E.S to provide:
//!
//! - **Interactive Coding**: Chat-based interface for code modifications
//! - **Self-Healing**: Automatic detection and repair of compilation errors, test failures, and warnings
//! - **Self-Improvement**: Code documentation, refactoring, and quality improvements
//! - **Rich TUI**: Beautiful terminal interface with syntax highlighting and streaming output
//!
//! ## Features
//!
//! - Native integration with A.R.E.S LLM providers (Ollama, OpenAI, LlamaCpp)
//! - Tool-based architecture for file manipulation, git, and cargo operations
//! - Support for multiple target projects (ares-server, self, or any Rust project)
//! - Configurable via `pawan.toml` or `ares.toml`
//!
//! ## Quick Start
//!
//! ```bash
//! # Interactive mode
//! pawan
//!
//! # Self-heal current project
//! pawan heal
//!
//! # Improve documentation
//! pawan improve docs
//!
//! # Execute a task
//! pawan task "add input validation to CreateAgentRequest"
//! ```

pub mod agent;
pub mod config;
pub mod healing;
pub mod tools;

#[cfg(feature = "tui")]
pub mod tui;

pub use agent::PawanAgent;
pub use config::PawanConfig;

/// Error types for Pawan
#[derive(Debug, thiserror::Error)]
pub enum PawanError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Tool execution error: {0}")]
    Tool(String),

    #[error("Agent error: {0}")]
    Agent(String),

    #[error("LLM error: {0}")]
    Llm(String),

    #[error("Git error: {0}")]
    Git(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Not found: {0}")]
    NotFound(String),
}

/// Result type alias for Pawan operations
pub type Result<T> = std::result::Result<T, PawanError>;

/// Version information
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default model for coding tasks (Nemotron reasoning model)
pub const DEFAULT_MODEL: &str = "nemotron";

/// Maximum iterations for tool calling loops
pub const MAX_TOOL_ITERATIONS: usize = 50;

/// Default timeout for bash commands (in seconds)
pub const DEFAULT_BASH_TIMEOUT: u64 = 120;
