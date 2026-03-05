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
#[cfg(feature = "lancedb")]
pub mod lancedb;
#[cfg(feature = "pgvector")]
pub mod pgvector;
#[cfg(feature = "pinecone")]
pub mod pinecone;
#[cfg(feature = "qdrant")]
pub mod qdrant;

// Relational database
/// Database traits and common types shared across providers.
pub mod traits;
/// Turso/libSQL database client implementation.
pub mod postgres;
/// Multi-tenant tenant management.
pub mod tenants;

// Re-exports
pub use vectorstore::{CollectionInfo, CollectionStats, VectorStore, VectorStoreProvider};

#[cfg(feature = "ares-vector")]
pub use ares_vector::AresVectorStore;
#[cfg(feature = "lancedb")]
pub use lancedb::LanceDBStore;
#[cfg(feature = "qdrant")]
pub use qdrant::QdrantVectorStore;
pub use postgres::PostgresClient;
pub use tenants::{TenantDb, UsageSummary};
