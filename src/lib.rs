//! # A.R.E.S - Agentic Retrieval Enhanced Server
//!
//! A production-grade agentic chatbot server built in Rust with multi-provider
//! LLM support, tool calling, RAG, MCP integration, and advanced research capabilities.
//!
//! ## Overview
//!
//! A.R.E.S can be used in two ways:
//!
//! 1. **As a standalone server** - Run the `ares-server` binary
//! 2. **As a library** - Import components into your own Rust project
//!
//! ## Quick Start (Library Usage)
//!
//! Add to your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! ares-server = "0.2"
//! ```
//!
//! ### Basic Example
//!
//! ```rust,ignore
//! use ares::{Provider, LLMClient};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create an Ollama provider
//!     let provider = Provider::Ollama {
//!         base_url: "http://localhost:11434".to_string(),
//!         model: "llama3.2:3b".to_string(),
//!     };
//!
//!     // Create a client and generate a response
//!     let client = provider.create_client().await?;
//!     let response = client.generate("Hello, world!").await?;
//!     println!("{}", response);
//!
//!     Ok(())
//! }
//! ```
//!
//! ### Using Tools
//!
//! ```rust,ignore
//! use ares::{ToolRegistry, tools::calculator::Calculator};
//! use std::sync::Arc;
//!
//! let mut registry = ToolRegistry::new();
//! registry.register(Arc::new(Calculator));
//!
//! // Tools can be used with LLM function calling
//! let tool_definitions = registry.definitions();
//! ```
//!
//! ### Configuration-Driven Setup
//!
//! ```rust,ignore
//! use ares::{AresConfigManager, AgentRegistry, ProviderRegistry, ToolRegistry};
//! use std::sync::Arc;
//!
//! // Load configuration from ares.toml
//! let config_manager = AresConfigManager::new("ares.toml")?;
//! let config = config_manager.config();
//!
//! // Create registries from configuration
//! let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
//! let tool_registry = Arc::new(ToolRegistry::with_config(&config));
//! let agent_registry = AgentRegistry::from_config(
//!     &config,
//!     provider_registry,
//!     tool_registry,
//! );
//! ```
//!
//! ## Feature Flags
//!
//! | Feature | Description |
//! |---------|-------------|
//! | `ollama` | Ollama local inference (default) |
//! | `openai` | OpenAI API support |
//! | `llamacpp` | Direct GGUF model loading |
//! | `local-db` | Local SQLite database (default) |
//! | `turso` | Remote Turso database |
//! | `qdrant` | Qdrant vector database |
//! | `mcp` | Model Context Protocol support |
//!
//! ## Modules
//!
//! - [`agents`] - Agent framework for multi-agent orchestration
//! - [`api`] - REST API handlers and routes
//! - [`auth`] - JWT authentication and middleware
//! - [`db`] - Database abstraction (SQLite, Turso)
//! - [`llm`] - LLM client implementations
//! - [`tools`] - Tool definitions and registry
//! - [`workflows`] - Declarative workflow engine
//! - [`types`] - Common types and error handling
//!
//! ## Architecture
//!
//! A.R.E.S uses a hybrid configuration system:
//!
//! - **TOML** (`ares.toml`): Infrastructure config (server, auth, providers)
//! - **TOON** (`config/*.toon`): Behavioral config (agents, models, tools)
//!
//! Both support hot-reloading for zero-downtime configuration changes.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
#![warn(rustdoc::missing_crate_level_docs)]

/// AI agent orchestration and management.
pub mod agents;
/// HTTP API handlers and routes.
pub mod api;
/// JWT authentication and middleware.
pub mod auth;
/// Database clients (Turso/SQLite, Qdrant).
pub mod db;
/// LLM provider clients and abstractions.
pub mod llm;
/// Model Context Protocol (MCP) server integration.
#[cfg(feature = "mcp")]
pub mod mcp;
/// Conversation memory and context management.
pub mod memory;
/// Retrieval Augmented Generation (RAG) components.
pub mod rag;
/// Multi-agent research coordination.
pub mod research;
/// Built-in tools (calculator, web search).
pub mod tools;
/// Core types (requests, responses, errors).
pub mod types;
/// Configuration utilities (TOML, TOON).
pub mod utils;
/// Workflow engine for agent orchestration.
pub mod workflows;

// Re-export commonly used types
pub use agents::{AgentRegistry, AgentRegistryBuilder};
pub use db::TursoClient;
pub use llm::client::LLMClientFactoryTrait;
pub use llm::{
    ConfigBasedLLMFactory, LLMClient, LLMClientFactory, LLMResponse, Provider, ProviderRegistry,
};
pub use tools::registry::ToolRegistry;
pub use types::{AppError, Result};
pub use utils::toml_config::{AresConfig, AresConfigManager};
pub use utils::toon_config::DynamicConfigManager;
pub use workflows::{WorkflowEngine, WorkflowOutput, WorkflowStep};

use crate::auth::jwt::AuthService;
use std::sync::Arc;

/// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    /// TOML-based infrastructure configuration with hot-reload support
    pub config_manager: Arc<AresConfigManager>,
    /// TOON-based dynamic behavioral configuration with hot-reload support
    pub dynamic_config: Arc<DynamicConfigManager>,
    /// Database client
    pub turso: Arc<TursoClient>,
    /// LLM client factory (config-based)
    pub llm_factory: Arc<ConfigBasedLLMFactory>,
    /// Provider registry for model/provider management
    pub provider_registry: Arc<ProviderRegistry>,
    /// Agent registry for creating config-driven agents
    pub agent_registry: Arc<AgentRegistry>,
    /// Tool registry for agent tools
    pub tool_registry: Arc<ToolRegistry>,
    /// Authentication service
    pub auth_service: Arc<AuthService>,
}
