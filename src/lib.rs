//! ARES - Agentic Retrieval Enhanced Server
//!
//! A production-grade agentic chatbot server with multi-provider LLM support,
//! local-first operation, and MCP integration.
//!
//! ## Features
//!
//! - **Multi-provider LLM Support**: OpenAI, Ollama, LlamaCpp, and more
//! - **Local-first Operation**: Works completely offline with local databases and models
//! - **MCP Integration**: Model Context Protocol support via daedra
//! - **Vector Search**: Local in-memory vector store for RAG
//! - **Agent System**: Multiple specialized agents for different tasks
//!
//! ## Modules
//!
//! - [`agents`] - Agent implementations for various tasks
//! - [`api`] - HTTP API handlers and routes
//! - [`auth`] - Authentication and authorization
//! - [`db`] - Database clients (Turso/libSQL, local vector store)
//! - [`llm`] - LLM client implementations and traits
//! - [`mcp`] - Model Context Protocol server
//! - [`memory`] - Context and memory management
//! - [`rag`] - Retrieval Augmented Generation components
//! - [`research`] - Deep research coordination
//! - [`tools`] - Tool definitions and registry
//! - [`types`] - Common types and data structures
//! - [`utils`] - Utility functions and configuration

use std::sync::Arc;

pub mod agents;
pub mod api;
pub mod auth;
pub mod db;
pub mod llm;
pub mod mcp;
pub mod memory;
pub mod rag;
pub mod research;
pub mod tools;
pub mod types;
pub mod utils;

// Re-export commonly used types and traits
pub use db::{QdrantClient, TursoClient};
pub use llm::{LLMClient, LLMClientFactory, Provider};
pub use types::{AppError, Result};

/// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<utils::config::Config>,
    pub turso: Arc<TursoClient>,
    pub qdrant: Arc<QdrantClient>,
    pub llm_factory: Arc<LLMClientFactory>,
    pub auth_service: Arc<auth::jwt::AuthService>,
}
