//! Database clients and vector stores.
//!
//! This module provides database abstractions for:
//! - **Turso/SQLite**: Relational database for conversations, users, etc.
//! - **Vector Stores**: Multi-provider vector database support
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

#![allow(missing_docs)]

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
pub mod traits;
pub mod turso;

// Re-exports
pub use vectorstore::{CollectionInfo, CollectionStats, VectorStore, VectorStoreProvider};

#[cfg(feature = "ares-vector")]
pub use ares_vector::AresVectorStore;
#[cfg(feature = "lancedb")]
pub use lancedb::LanceDBStore;
#[cfg(feature = "qdrant")]
pub use qdrant::QdrantClient;
pub use turso::TursoClient;
