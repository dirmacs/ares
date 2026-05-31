//! Database Clients and Vector Stores
//!
//! This module provides database abstractions for:
//! - **PostgreSQL**: Relational database for conversations, users, etc.
//! - **Vector Stores**: Multi-provider vector database support
//!
//! # Relational Database
//!
//! The [`PostgresClient`] provides async access to PostgreSQL for:
//! - User management (registration, authentication)
//! - Conversation storage and retrieval
//! - Message history
//! - User memory (facts, preferences)
//!
//! # Vector Store Providers
//!
//! The following vector store backends are supported:
//! - `ares-vector` (default) - Pure Rust embedded HNSW vector database
//! - `lancedb` - Serverless, embedded vector database (may have build issues on Windows)
//! - `qdrant` - High-performance vector search engine
//! - `pgvector` - PostgreSQL extension
//! - `chromadb` - Simple embedding database
//! - `pinecone` - Managed cloud service
//!
//! Enable providers via Cargo features:
//! ```toml
//! ares = { version = "*", features = ["ares-vector", "qdrant"] }
//! ```
//!
//! # Example
//!
//! ```ignore
//! use ares::db::{PostgresClient, VectorStore, AresVectorStore};
//!
//! // Relational database
//! let db = PostgresClient::new("postgres://user:pass@localhost:5432/ares").await?;
//! let user = db.get_user_by_id(user_id).await?;
//!
//! // Vector store
//! let vector_store = AresVectorStore::new("./vectors").await?;
//! vector_store.upsert("docs", embeddings, metadata).await?;
//! let results = vector_store.search("docs", query_embedding, 10).await?;
//! ```

// Vector store abstraction layer
pub mod vectorstore;

// Provider implementations
#[cfg(feature = "ares-vector")]
pub mod ares_vector;
#[cfg(feature = "chromadb")]
pub mod chromadb;
#[cfg(any(feature = "lancedb", feature = "postgres"))]
pub mod lancedb;
#[cfg(any(feature = "pgvector", feature = "postgres"))]
pub mod pgvector;
#[cfg(any(feature = "pinecone", feature = "postgres"))]
pub mod pinecone;
#[cfg(any(feature = "qdrant", feature = "postgres"))]
pub mod qdrant;

// Relational database (requires postgres feature for sqlx)
#[cfg(feature = "postgres")]
/// Agent run tracking (execution history).
pub mod agent_runs;
#[cfg(feature = "postgres")]
/// Reviewer and quality feedback attached to agent runs.
pub mod agent_feedback;
#[cfg(feature = "postgres")]
/// Platform alerts (health, quota, errors).
pub mod alerts;
#[cfg(feature = "postgres")]
/// Admin audit log (mutation tracking).
pub mod audit_log;
#[cfg(feature = "postgres")]
/// PostgreSQL database client implementation.
pub mod postgres;
#[cfg(feature = "postgres")]
/// Per-tenant agent instance management.
pub mod tenant_agents;
#[cfg(feature = "postgres")]
/// Multi-tenant tenant management.
pub mod tenants;
/// Database traits and common types shared across providers.
#[cfg(feature = "postgres")]
pub mod traits;
/// Turso/libSQL database client (alternative to PostgreSQL).
#[cfg(feature = "turso")]
pub mod turso;
#[cfg(feature = "postgres")]
/// Agent config version history (Sprint 11).
pub mod agent_versions;
#[cfg(feature = "postgres")]
/// Pure SQL builders and row conversions (testable without a live DB).
pub mod query_builders;

// Re-exports
pub use vectorstore::{CollectionInfo, CollectionStats, VectorStore, VectorStoreProvider};

#[cfg(feature = "ares-vector")]
pub use ares_vector::AresVectorStore;
#[cfg(feature = "lancedb")]
pub use lancedb::LanceDBStore;
#[cfg(feature = "postgres")]
pub use postgres::PostgresClient;
#[cfg(feature = "turso")]
pub use turso::TursoClient;
#[cfg(feature = "qdrant")]
pub use qdrant::QdrantVectorStore;
#[cfg(feature = "postgres")]
pub use tenants::{TenantDb, UsageSummary};
