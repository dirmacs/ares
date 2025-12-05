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

// Re-export commonly used types
pub use db::TursoClient;
pub use llm::{LLMClient, LLMClientFactory, LLMResponse, Provider};
pub use types::{AppError, Result};

use crate::{auth::jwt::AuthService, utils::config::Config};
use std::sync::Arc;

/// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub turso: Arc<TursoClient>,
    pub llm_factory: Arc<LLMClientFactory>,
    pub auth_service: Arc<AuthService>,
}
