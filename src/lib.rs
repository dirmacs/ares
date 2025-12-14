//! A.R.E.S - Agentic Retrieval Enhanced Server
//!
//! A production-grade agentic chatbot server built in Rust with multi-provider
//! LLM support, tool calling, RAG, MCP integration, and advanced research capabilities.

pub mod agents;
pub mod api;
pub mod auth;
pub mod db;
pub mod llm;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod memory;
pub mod rag;
pub mod research;
pub mod tools;
pub mod types;
pub mod utils;
pub mod workflows;

// Re-export commonly used types
pub use agents::{AgentRegistry, AgentRegistryBuilder};
pub use db::TursoClient;
pub use llm::client::LLMClientFactoryTrait;
pub use llm::{ConfigBasedLLMFactory, LLMClient, LLMClientFactory, LLMResponse, Provider, ProviderRegistry};
pub use tools::registry::ToolRegistry;
pub use types::{AppError, Result};
pub use utils::toml_config::{AresConfig, AresConfigManager};
pub use workflows::{WorkflowEngine, WorkflowOutput, WorkflowStep};

use crate::auth::jwt::AuthService;
use std::sync::Arc;

/// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    /// TOML-based configuration with hot-reload support
    pub config_manager: Arc<AresConfigManager>,
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
