//! API request handlers.
//!
//! This module contains all HTTP request handlers organized by functionality.

/// Admin tenant management handlers.
// cordis Phase6: explicit path allows both admin.rs and admin/mod.rs to coexist (E0761 bypass)
#[path = "admin.rs"]
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
#[cfg(feature = "ares-vector")]
pub mod rag;
/// Research coordination handlers.
pub mod research;
/// Skills discovery handlers — runtime-gated via SkillsService check (was feature-gated).
pub mod skills;
/// User-created agent management handlers.
pub mod user_agents;
/// V1 API key-authenticated tenant-scoped handlers.
// cordis Phase6: explicit path allows both v1.rs and v1/mod.rs to coexist
#[path = "v1.rs"]
pub mod v1;
/// Workflow execution handlers.
pub mod workflows;
/// Cordis health_context handler — State with Context proof (shim phase).
/// Runtime-gated via PostgresService check (was feature-gated).
pub mod health_context;
