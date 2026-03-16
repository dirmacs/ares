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

use crate::types::{AppError, Result};

/// ChromaDB vector store (not yet implemented).
///
/// This struct will provide integration with ChromaDB's embedding
/// database for storing and querying embeddings.
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
             See https://github.com/dirmacs/ares for implementation status.".to_string()
        ))
    }
}
