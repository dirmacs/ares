//! ChromaDB vector database integration.
//!
//! This module provides integration with ChromaDB, an open-source embedding database.
//!
//! # Status
//!
//! **Not yet implemented.** This is a placeholder for future development.
//!
//! # Feature Flag
//!
//! Enable with `--features chromadb`
//!
//! # Future Implementation
//!
//! When implemented, this will support:
//! - Creating and managing collections
//! - Adding documents with automatic embedding
//! - Similarity search with metadata filtering
//! - Integration with ChromaDB's Python server
//!
//! # Example (future API)
//!
//! ```rust,ignore
//! use ares::db::ChromaDBStore;
//!
//! let store = ChromaDBStore::new("http://localhost:8000").await?;
//! store.create_collection("documents").await?;
//! store.add("documents", &texts, &metadatas).await?;
//! let results = store.query("documents", "search query", 10).await?;
//! ```

use ares_types::types::{AppError, Result};

/// ChromaDB vector store (not yet implemented).
///
/// This struct will provide integration with ChromaDB's embedding
/// database for storing and querying embeddings.
#[derive(Debug)]
pub struct ChromaDBStore {
    _private: (),
}

impl ChromaDBStore {
    /// Create a new ChromaDBStore.
    ///
    /// # Errors
    ///
    /// Currently always returns an error as this feature is not yet implemented.
    pub async fn new(_host: &str) -> Result<Self> {
        Err(AppError::Configuration(
            "ChromaDBStore is not yet implemented. Use 'ares-vector' (default) or 'qdrant' instead. \
             See https://github.com/dirmacs/ares for implementation status.".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::AppError;

    #[tokio::test]
    async fn new_returns_configuration_error() {
        let err = ChromaDBStore::new("http://localhost:8000")
            .await
            .unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if {
            msg.contains("not yet implemented") && msg.contains("ares-vector")
        });
    }

    #[tokio::test]
    async fn new_error_mentions_alternatives() {
        let err = ChromaDBStore::new("http://127.0.0.1:8000")
            .await
            .unwrap_err();
        let msg = match err {
            AppError::Configuration(m) => m,
            other => panic!("expected Configuration, got {other:?}"),
        };
        assert!(msg.contains("qdrant"), "should suggest qdrant: {msg}");
    }

    #[tokio::test]
    async fn new_ignores_host_but_still_errors() {
        for host in ["", "https://chromadb.example", "localhost:8000"] {
            let err = ChromaDBStore::new(host).await.unwrap_err();
            matches::assert_matches!(err, AppError::Configuration(_));
        }
    }
}
