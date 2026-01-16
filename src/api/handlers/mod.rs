//! API request handlers.
//!
//! This module contains all HTTP request handlers organized by functionality.

/// Agent listing and info handlers.
pub mod agents;
/// Authentication handlers (login, register).
pub mod auth;
/// Chat and streaming handlers.
pub mod chat;
/// Conversation CRUD handlers.
pub mod conversations;
/// RAG (document ingestion/search) handlers.
pub mod rag;
/// Research coordination handlers.
pub mod research;
/// User-created agent management handlers.
pub mod user_agents;
/// Workflow execution handlers.
pub mod workflows;
