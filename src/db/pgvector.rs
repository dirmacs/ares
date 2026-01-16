//! PostgreSQL pgvector integration.
//!
//! This module provides vector similarity search using PostgreSQL with the pgvector extension.
//!
//! # Status
//!
//! **Not yet implemented.** This is a placeholder for future development.
//!
//! # Feature Flag
//!
//! Enable with `--features pgvector`
//!
//! # Future Implementation
//!
//! When implemented, this will support:
//! - Creating vector collections as PostgreSQL tables
//! - Storing embeddings with metadata
//! - Similarity search using pgvector's IVFFlat and HNSW indexes
//! - Filtering by metadata
//!
//! # Example (future API)
//!
//! ```rust,ignore
//! use ares::db::PgVectorStore;
//!
//! let store = PgVectorStore::new("postgres://localhost/vectors").await?;
//! store.create_collection("documents", 384).await?;
//! store.upsert("documents", &embedding, &metadata).await?;
//! let results = store.search("documents", &query_embedding, 10).await?;
//! ```

use crate::types::{AppError, Result};

/// PostgreSQL pgvector store (not yet implemented).
///
/// This struct will provide vector similarity search using PostgreSQL
/// with the pgvector extension.
pub struct PgVectorStore {
    _private: (),
}

impl PgVectorStore {
    /// Create a new PgVectorStore.
    ///
    /// # Errors
    ///
    /// Currently always returns an error as this feature is not yet implemented.
    pub async fn new(_connection_string: &str) -> Result<Self> {
        Err(AppError::Configuration(
            "PgVectorStore is not yet implemented. Use 'ares-vector' (default) or 'qdrant' instead. \
             See https://github.com/dirmacs/ares for implementation status.".to_string()
        ))
    }
}
