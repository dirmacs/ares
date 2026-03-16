//! API request handlers.
//!
//! This module contains all HTTP request handlers organized by functionality.

/// Admin tenant management handlers.
pub mod admin;
/// Public (no auth) handlers — e.g. lead capture.
pub mod public;
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
/// RAG (document ingestion/search) handlers.
/// Requires the `local-embeddings` feature (for ONNX-based embeddings) and
/// `ares-vector` feature (for the embedded vector database).
#[cfg(all(feature = "local-embeddings", feature = "ares-vector"))]
pub mod rag;
/// Research coordination handlers.
pub mod research;
/// User-created agent management handlers.
pub mod user_agents;
/// V1 API key-authenticated tenant-scoped handlers.
pub mod v1;
/// Workflow execution handlers.
pub mod workflows;
