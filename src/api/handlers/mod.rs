//! API request handlers.
//!
//! This module contains all HTTP request handlers organized by functionality.

/// Admin tenant management handlers.
pub mod admin;
/// Agent listing and info handlers.
pub mod agents;
/// Authentication handlers (login, register).
pub mod auth;
/// Chat and streaming handlers.
pub mod chat;
/// Conversation CRUD handlers.
pub mod conversations;
/// Deployment automation handlers.
pub mod deploy;
/// Document-upload trigger handlers.
pub mod document_upload;
/// Field-change trigger handlers.
pub mod field_change;
/// Loop-mode agent lifecycle handlers (start/list/stop).
pub mod loops;
/// RAG (document ingestion/search) handlers.
#[cfg(all(feature = "local-embeddings", feature = "ares-vector"))]
pub mod rag;
/// Research coordination handlers.
pub mod research;
/// Skills discovery handlers (requires `skills` feature).
#[cfg(feature = "skills")]
pub mod skills;
/// User-created agent management handlers.
pub mod user_agents;
/// V1 API key-authenticated tenant-scoped handlers.
pub mod v1;
/// Workflow execution handlers.
pub mod workflows;
