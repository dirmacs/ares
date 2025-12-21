//! CLI module for A.R.E.S
//!
//! Provides command-line interface parsing and handling for the ares-server binary.
//! Uses clap for argument parsing and owo-colors for colored terminal output.

pub mod init;
pub mod output;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// A.R.E.S - Agentic Retrieval Enhanced Server
///
/// A production-grade agentic chatbot server with multi-provider LLM support,
/// tool calling, RAG, and MCP integration.
#[derive(Parser, Debug)]
#[command(
    name = "ares-server",
    author = "Dirmacs <build@dirmacs.com>",
    version,
    about = "A.R.E.S - Agentic Retrieval Enhanced Server",
    long_about = "A production-grade agentic chatbot server with multi-provider LLM support,\n\
                  tool calling, RAG (Retrieval Augmented Generation), and MCP integration.\n\n\
                  Run without arguments to start the server, or use 'init' to scaffold a new project.",
    after_help = "EXAMPLES:\n    \
                  ares-server init              # Scaffold a new A.R.E.S project\n    \
                  ares-server init --minimal    # Scaffold with minimal configuration\n    \
                  ares-server                   # Start the server (requires ares.toml)\n    \
                  ares-server --config my.toml  # Use a custom config file"
)]
pub struct Cli {
    /// Path to the configuration file
    #[arg(short, long, default_value = "ares.toml", global = true)]
    pub config: PathBuf,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Subcommand to execute
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Available CLI subcommands
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize a new A.R.E.S project with configuration files
    ///
    /// Creates ares.toml and the config/ directory structure with
    /// all necessary files for running an A.R.E.S server.
    Init {
        /// Directory to initialize (defaults to current directory)
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Overwrite existing files without prompting
        #[arg(short, long)]
        force: bool,

        /// Create a minimal configuration (fewer agents and tools)
        #[arg(short, long)]
        minimal: bool,

        /// Skip creating example TOON files in config/
        #[arg(long)]
        no_examples: bool,

        /// LLM provider to configure (ollama, openai, or both)
        #[arg(long, default_value = "ollama")]
        provider: String,

        /// Host address for the server
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Port for the server
        #[arg(long, default_value = "3000")]
        port: u16,
    },

    /// Show configuration information
    Config {
        /// Show the full configuration
        #[arg(short = 'f', long)]
        full: bool,

        /// Validate the configuration file
        #[arg(long)]
        validate: bool,
    },

    /// Manage agents
    #[command(subcommand)]
    Agent(AgentCommands),
}

/// Agent management subcommands
#[derive(Subcommand, Debug)]
pub enum AgentCommands {
    /// List all configured agents
    List,

    /// Show details for a specific agent
    Show {
        /// Name of the agent
        name: String,
    },
}

impl Cli {
    /// Parse CLI arguments
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
