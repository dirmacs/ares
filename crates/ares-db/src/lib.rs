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

#[cfg(test)]
mod tests {
    use super::{CollectionInfo, CollectionStats};
    #[cfg(feature = "ares-vector")]
    use super::VectorStoreProvider;
    #[cfg(feature = "ares-vector")]
    use serde_json::json;

    #[test]
    fn collection_stats_serde_roundtrip() {
        let stats = CollectionStats {
            name: "docs".into(),
            document_count: 42,
            dimensions: 384,
            index_size_bytes: Some(1024),
            distance_metric: "cosine".into(),
        };
        let value = serde_json::to_value(&stats).expect("serialize");
        let back: CollectionStats = serde_json::from_value(value).expect("deserialize");
        assert_eq!(back.name, "docs");
        assert_eq!(back.document_count, 42);
        assert_eq!(back.dimensions, 384);
    }

    #[test]
    fn collection_info_serde_roundtrip() {
        let info = CollectionInfo {
            name: "embeddings".into(),
            dimensions: 768,
            document_count: 10,
        };
        let json = serde_json::to_string(&info).expect("serialize");
        let back: CollectionInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.name, "embeddings");
        assert_eq!(back.dimensions, 768);
    }

    #[cfg(feature = "ares-vector")]
    #[test]
    fn ares_vector_provider_tagged_json() {
        let provider = VectorStoreProvider::AresVector {
            path: Some("./data/vectors".into()),
        };
        let value = serde_json::to_value(&provider).expect("serialize");
        assert_eq!(value["provider"], json!("aresvector"));
    }
}
