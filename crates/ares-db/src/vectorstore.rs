//! Vector Store Abstraction Layer
//!
//! This module provides a unified interface for vector database operations,
//! allowing the application to work with multiple vector store backends
//! (LanceDB, Qdrant, pgvector, ChromaDB, Pinecone) through a common trait.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      VectorStore Trait                       │
//! ├─────────────────────────────────────────────────────────────┤
//! │  create_collection  │  search  │  upsert  │  delete  │ ... │
//! └─────────────────────────────────────────────────────────────┘
//!          ▲                ▲            ▲           ▲
//!          │                │            │           │
//!    ┌─────┴────┐    ┌─────┴────┐  ┌────┴────┐  ┌───┴────┐
//!    │ LanceDB  │    │  Qdrant  │  │pgvector │  │Pinecone│
//!    │ (default)│    │          │  │         │  │(cloud) │
//!    └──────────┘    └──────────┘  └─────────┘  └────────┘
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::db::vectorstore::{VectorStore, VectorStoreProvider};
//!
//! // Create a LanceDB store (default, local-first)
//! let store = VectorStoreProvider::LanceDB {
//!     path: "./data/lancedb".into(),
//! }.create_store().await?;
//!
//! // Create a collection
//! store.create_collection("documents", 384).await?;
//!
//! // Upsert documents with embeddings
//! store.upsert("documents", &documents).await?;
//!
//! // Search
//! let results = store.search("documents", &query_embedding, 10, 0.5).await?;
//! ```

use ares_types::types::{AppError, Document, Result, SearchResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ============================================================================
// Vector Store Provider Configuration
// ============================================================================

/// Configuration for vector store providers.
///
/// Each variant contains the necessary configuration to connect to
/// a specific vector database backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum VectorStoreProvider {
    /// AresVector - Pure Rust embedded vector database with HNSW (default).
    ///
    /// No native dependencies, compiles anywhere Rust does.
    /// Data stored locally with optional persistence.
    #[cfg(feature = "ares-vector")]
    AresVector {
        /// Path to the data directory (None for in-memory).
        path: Option<String>,
    },

    /// LanceDB - Serverless, embedded vector database.
    ///
    /// No separate server process required. Data stored locally.
    /// Note: May have build issues on Windows due to protoc dependency.
    #[cfg(feature = "lancedb")]
    LanceDB {
        /// Path to the LanceDB storage directory.
        path: String,
    },

    /// Qdrant - High-performance vector search engine.
    ///
    /// Requires a running Qdrant server.
    #[cfg(feature = "qdrant")]
    Qdrant {
        /// Qdrant server URL (e.g., "http://localhost:6334").
        url: String,
        /// Optional API key for authentication.
        api_key: Option<String>,
    },

    /// pgvector - PostgreSQL extension for vector similarity search.
    ///
    /// Requires PostgreSQL with pgvector extension installed.
    #[cfg(feature = "pgvector")]
    PgVector {
        /// PostgreSQL connection string.
        connection_string: String,
    },

    /// ChromaDB - Simple, open-source embedding database.
    ///
    /// Requires a running ChromaDB server.
    #[cfg(feature = "chromadb")]
    ChromaDB {
        /// ChromaDB server URL (e.g., "http://localhost:8000").
        url: String,
    },

    /// Pinecone - Managed cloud vector database.
    ///
    /// Cloud-only, requires API key and environment configuration.
    #[cfg(feature = "pinecone")]
    Pinecone {
        /// Pinecone API key.
        api_key: String,
        /// Pinecone environment (e.g., "us-east-1").
        environment: String,
        /// Index name to use.
        index_name: String,
    },

    /// In-memory vector store for testing.
    ///
    /// Data is not persisted and will be lost when the process exits.
    InMemory,
}

impl VectorStoreProvider {
    /// Create a vector store instance from this provider configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails or the provider
    /// feature is not enabled.
    pub async fn create_store(&self) -> Result<Box<dyn VectorStore>> {
        match self {
            #[cfg(feature = "ares-vector")]
            VectorStoreProvider::AresVector { path } => {
                let store = super::ares_vector::AresVectorStore::new(path.clone()).await?;
                Ok(Box::new(store))
            }

            #[cfg(feature = "lancedb")]
            VectorStoreProvider::LanceDB { path } => {
                let store = super::lancedb::LanceDBStore::new(path).await?;
                Ok(Box::new(store))
            }

            #[cfg(feature = "qdrant")]
            VectorStoreProvider::Qdrant { url, api_key } => {
                let store =
                    super::qdrant::QdrantVectorStore::new(url.clone(), api_key.clone()).await?;
                Ok(Box::new(store))
            }

            #[cfg(feature = "pgvector")]
            VectorStoreProvider::PgVector { connection_string } => {
                let store = super::pgvector::PgVectorStore::new(connection_string).await?;
                Ok(Box::new(store))
            }

            #[cfg(feature = "chromadb")]
            VectorStoreProvider::ChromaDB { url } => {
                let store = super::chromadb::ChromaDBStore::new(url).await?;
                Ok(Box::new(store))
            }

            #[cfg(feature = "pinecone")]
            VectorStoreProvider::Pinecone {
                api_key,
                environment,
                index_name,
            } => {
                let store =
                    super::pinecone::PineconeStore::new(api_key, environment, index_name).await?;
                Ok(Box::new(store))
            }

            VectorStoreProvider::InMemory => {
                let store = InMemoryVectorStore::new();
                Ok(Box::new(store))
            }

            #[allow(unreachable_patterns)]
            _ => Err(AppError::Configuration(
                "Vector store provider not enabled. Check feature flags.".into(),
            )),
        }
    }

    /// Create a provider from environment variables.
    ///
    /// Checks for provider-specific environment variables in order:
    /// 1. `ARES_VECTOR_PATH` → AresVector (default)
    /// 2. `LANCEDB_PATH` → LanceDB
    /// 3. `QDRANT_URL` → Qdrant
    /// 4. `PGVECTOR_URL` → pgvector
    /// 5. `CHROMADB_URL` → ChromaDB
    /// 6. `PINECONE_API_KEY` → Pinecone
    /// 7. Falls back to AresVector in-memory or InMemory
    pub fn from_env() -> Self {
        #[cfg(feature = "ares-vector")]
        if let Ok(path) = std::env::var("ARES_VECTOR_PATH") {
            return VectorStoreProvider::AresVector { path: Some(path) };
        }

        #[cfg(feature = "lancedb")]
        if let Ok(path) = std::env::var("LANCEDB_PATH") {
            return VectorStoreProvider::LanceDB { path };
        }

        #[cfg(feature = "qdrant")]
        if let Ok(url) = std::env::var("QDRANT_URL") {
            let api_key = std::env::var("QDRANT_API_KEY").ok();
            return VectorStoreProvider::Qdrant { url, api_key };
        }

        #[cfg(feature = "pgvector")]
        if let Ok(connection_string) = std::env::var("PGVECTOR_URL") {
            return VectorStoreProvider::PgVector { connection_string };
        }

        #[cfg(feature = "chromadb")]
        if let Ok(url) = std::env::var("CHROMADB_URL") {
            return VectorStoreProvider::ChromaDB { url };
        }

        #[cfg(feature = "pinecone")]
        if let Ok(api_key) = std::env::var("PINECONE_API_KEY") {
            let environment =
                std::env::var("PINECONE_ENVIRONMENT").unwrap_or_else(|_| "us-east-1".into());
            let index_name =
                std::env::var("PINECONE_INDEX").unwrap_or_else(|_| "ares-documents".into());
            return VectorStoreProvider::Pinecone {
                api_key,
                environment,
                index_name,
            };
        }

        // Default: prefer ares-vector (in-memory) if available, else basic InMemory
        #[cfg(feature = "ares-vector")]
        return VectorStoreProvider::AresVector { path: None };

        #[cfg(not(feature = "ares-vector"))]
        VectorStoreProvider::InMemory
    }
}

// ============================================================================
// Collection Statistics
// ============================================================================

/// Statistics about a vector collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionStats {
    /// Name of the collection.
    pub name: String,
    /// Number of documents/vectors in the collection.
    pub document_count: usize,
    /// Dimensionality of vectors in the collection.
    pub dimensions: usize,
    /// Size of the index in bytes (if available).
    pub index_size_bytes: Option<u64>,
    /// Distance metric used (e.g., "cosine", "euclidean").
    pub distance_metric: String,
}

/// Information about a collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionInfo {
    /// Name of the collection.
    pub name: String,
    /// Number of documents in the collection.
    pub document_count: usize,
    /// Vector dimensions.
    pub dimensions: usize,
}

// ============================================================================
// Vector Store Trait
// ============================================================================

/// Abstract trait for vector database operations.
///
/// This trait defines a common interface for all vector store backends,
/// enabling the application to work with different databases interchangeably.
///
/// # Implementors
///
/// - `LanceDBStore` - Serverless, embedded (default)
/// - `QdrantVectorStore` - High-performance server
/// - `PgVectorStore` - PostgreSQL extension
/// - `ChromaDBStore` - Simple embedding database
/// - `PineconeStore` - Managed cloud service
/// - `InMemoryVectorStore` - Testing only
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Get the name of this vector store provider.
    fn provider_name(&self) -> &'static str;

    /// Create a new collection with the specified vector dimensions.
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the collection to create.
    /// * `dimensions` - Dimensionality of vectors (e.g., 384 for BGE-small).
    ///
    /// # Errors
    ///
    /// Returns an error if the collection already exists or creation fails.
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()>;

    /// Delete a collection and all its data.
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the collection to delete.
    ///
    /// # Errors
    ///
    /// Returns an error if the collection doesn't exist or deletion fails.
    async fn delete_collection(&self, name: &str) -> Result<()>;

    /// List all collections in the vector store.
    async fn list_collections(&self) -> Result<Vec<CollectionInfo>>;

    /// Check if a collection exists.
    async fn collection_exists(&self, name: &str) -> Result<bool>;

    /// Get statistics about a collection.
    async fn collection_stats(&self, name: &str) -> Result<CollectionStats>;

    /// Upsert documents with their embeddings into a collection.
    ///
    /// Documents are identified by their `id` field. If a document with
    /// the same ID already exists, it will be updated.
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the collection.
    /// * `documents` - Documents to upsert (must have embeddings set).
    ///
    /// # Errors
    ///
    /// Returns an error if any document is missing an embedding or the
    /// upsert operation fails.
    async fn upsert(&self, collection: &str, documents: &[Document]) -> Result<usize>;

    /// Search for similar vectors in a collection.
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the collection to search.
    /// * `embedding` - Query vector to find similar documents.
    /// * `limit` - Maximum number of results to return.
    /// * `threshold` - Minimum similarity score (0.0 to 1.0).
    ///
    /// # Returns
    ///
    /// A vector of search results, sorted by similarity score (descending).
    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>>;

    /// Search with metadata filters.
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the collection to search.
    /// * `embedding` - Query vector.
    /// * `limit` - Maximum number of results.
    /// * `threshold` - Minimum similarity score.
    /// * `filters` - Metadata filters to apply.
    ///
    /// # Default Implementation
    ///
    /// Falls back to regular search if not overridden.
    async fn search_with_filters(
        &self,
        collection: &str,
        embedding: &[f32],
        limit: usize,
        threshold: f32,
        _filters: &[(String, String)],
    ) -> Result<Vec<SearchResult>> {
        // Default: ignore filters and do regular search
        // Providers should override this for proper filter support
        self.search(collection, embedding, limit, threshold).await
    }

    /// Delete documents by their IDs.
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the collection.
    /// * `ids` - IDs of documents to delete.
    ///
    /// # Returns
    ///
    /// Number of documents actually deleted.
    async fn delete(&self, collection: &str, ids: &[String]) -> Result<usize>;

    /// Get a document by ID.
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the collection.
    /// * `id` - Document ID.
    ///
    /// # Returns
    ///
    /// The document if found, or None.
    async fn get(&self, collection: &str, id: &str) -> Result<Option<Document>>;

    /// Count documents in a collection.
    async fn count(&self, collection: &str) -> Result<usize> {
        let stats = self.collection_stats(collection).await?;
        Ok(stats.document_count)
    }
}

// ============================================================================
// In-Memory Vector Store (for testing)
// ============================================================================

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// In-memory vector store for testing purposes.
///
/// Data is not persisted and will be lost when the process exits.
/// Uses cosine similarity for vector comparisons.
pub struct InMemoryVectorStore {
    collections: Arc<RwLock<HashMap<String, InMemoryCollection>>>,
}

struct InMemoryCollection {
    dimensions: usize,
    documents: HashMap<String, Document>,
}

impl InMemoryVectorStore {
    /// Create a new in-memory vector store.
    pub fn new() -> Self {
        Self {
            collections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Calculate cosine similarity between two vectors.
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() {
            return 0.0;
        }

        let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            return 0.0;
        }

        dot_product / (norm_a * norm_b)
    }
}

impl Default for InMemoryVectorStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VectorStore for InMemoryVectorStore {
    fn provider_name(&self) -> &'static str {
        "in-memory"
    }

    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        let mut collections = self.collections.write();
        if collections.contains_key(name) {
            return Err(AppError::InvalidInput(format!(
                "Collection '{}' already exists",
                name
            )));
        }
        collections.insert(
            name.to_string(),
            InMemoryCollection {
                dimensions,
                documents: HashMap::new(),
            },
        );
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        let mut collections = self.collections.write();
        collections
            .remove(name)
            .ok_or_else(|| AppError::NotFound(format!("Collection '{}' not found", name)))?;
        Ok(())
    }

    async fn list_collections(&self) -> Result<Vec<CollectionInfo>> {
        let collections = self.collections.read();
        Ok(collections
            .iter()
            .map(|(name, col)| CollectionInfo {
                name: name.clone(),
                document_count: col.documents.len(),
                dimensions: col.dimensions,
            })
            .collect())
    }

    async fn collection_exists(&self, name: &str) -> Result<bool> {
        let collections = self.collections.read();
        Ok(collections.contains_key(name))
    }

    async fn collection_stats(&self, name: &str) -> Result<CollectionStats> {
        let collections = self.collections.read();
        let col = collections
            .get(name)
            .ok_or_else(|| AppError::NotFound(format!("Collection '{}' not found", name)))?;

        Ok(CollectionStats {
            name: name.to_string(),
            document_count: col.documents.len(),
            dimensions: col.dimensions,
            index_size_bytes: None,
            distance_metric: "cosine".to_string(),
        })
    }

    async fn upsert(&self, collection: &str, documents: &[Document]) -> Result<usize> {
        let mut collections = self.collections.write();
        let col = collections
            .get_mut(collection)
            .ok_or_else(|| AppError::NotFound(format!("Collection '{}' not found", collection)))?;

        let mut count = 0;
        for doc in documents {
            if doc.embedding.is_none() {
                return Err(AppError::InvalidInput(format!(
                    "Document '{}' is missing embedding",
                    doc.id
                )));
            }
            col.documents.insert(doc.id.clone(), doc.clone());
            count += 1;
        }

        Ok(count)
    }

    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>> {
        let collections = self.collections.read();
        let col = collections
            .get(collection)
            .ok_or_else(|| AppError::NotFound(format!("Collection '{}' not found", collection)))?;

        let mut results: Vec<SearchResult> = col
            .documents
            .values()
            .filter_map(|doc| {
                let doc_embedding = doc.embedding.as_ref()?;
                let score = Self::cosine_similarity(embedding, doc_embedding);
                if score >= threshold {
                    Some(SearchResult {
                        document: Document {
                            id: doc.id.clone(),
                            content: doc.content.clone(),
                            metadata: doc.metadata.clone(),
                            embedding: None, // Don't return embeddings in results
                        },
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Sort by score descending
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Limit results
        results.truncate(limit);

        Ok(results)
    }

    async fn delete(&self, collection: &str, ids: &[String]) -> Result<usize> {
        let mut collections = self.collections.write();
        let col = collections
            .get_mut(collection)
            .ok_or_else(|| AppError::NotFound(format!("Collection '{}' not found", collection)))?;

        let mut count = 0;
        for id in ids {
            if col.documents.remove(id).is_some() {
                count += 1;
            }
        }

        Ok(count)
    }

    async fn get(&self, collection: &str, id: &str) -> Result<Option<Document>> {
        let collections = self.collections.read();
        let col = collections
            .get(collection)
            .ok_or_else(|| AppError::NotFound(format!("Collection '{}' not found", collection)))?;

        Ok(col.documents.get(id).cloned())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::DocumentMetadata;
    use chrono::Utc;

    fn clear_vector_env() {
        std::env::remove_var("ARES_VECTOR_PATH");
        std::env::remove_var("LANCEDB_PATH");
        std::env::remove_var("QDRANT_URL");
        std::env::remove_var("QDRANT_API_KEY");
        std::env::remove_var("PGVECTOR_URL");
        std::env::remove_var("CHROMADB_URL");
        std::env::remove_var("PINECONE_API_KEY");
        std::env::remove_var("PINECONE_ENVIRONMENT");
        std::env::remove_var("PINECONE_INDEX");
    }

    fn create_test_document(id: &str, content: &str, embedding: Vec<f32>) -> Document {
        Document {
            id: id.to_string(),
            content: content.to_string(),
            metadata: DocumentMetadata {
                title: format!("Test Doc {}", id),
                source: "test".to_string(),
                created_at: Utc::now(),
                tags: vec!["test".to_string()],
            },
            embedding: Some(embedding),
        }
    }

    // ===================== Original Tests =====================

    #[tokio::test]
    async fn test_inmemory_create_collection() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 384).await.unwrap();
        assert!(store.collection_exists("test").await.unwrap());
    }

    #[tokio::test]
    async fn test_inmemory_duplicate_collection_error() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 384).await.unwrap();
        let result = store.create_collection("test", 384).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_inmemory_upsert_and_search() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 3).await.unwrap();

        let doc1 = create_test_document("doc1", "Hello world", vec![1.0, 0.0, 0.0]);
        let doc2 = create_test_document("doc2", "Goodbye world", vec![0.0, 1.0, 0.0]);
        let doc3 = create_test_document("doc3", "Hello again", vec![0.9, 0.1, 0.0]);

        store.upsert("test", &[doc1, doc2, doc3]).await.unwrap();

        let results = store
            .search("test", &[1.0, 0.0, 0.0], 10, 0.5)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].document.id, "doc1");
        assert_eq!(results[1].document.id, "doc3");
    }

    #[tokio::test]
    async fn test_inmemory_delete() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 3).await.unwrap();

        let doc = create_test_document("doc1", "Test", vec![1.0, 0.0, 0.0]);
        store.upsert("test", &[doc]).await.unwrap();
        assert_eq!(store.count("test").await.unwrap(), 1);

        let deleted = store.delete("test", &["doc1".to_string()]).await.unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(store.count("test").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_inmemory_get() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 3).await.unwrap();

        let doc = create_test_document("doc1", "Test content", vec![1.0, 0.0, 0.0]);
        store.upsert("test", &[doc]).await.unwrap();

        let retrieved = store.get("test", "doc1").await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().content, "Test content");

        let not_found = store.get("test", "nonexistent").await.unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_inmemory_list_collections() {
        let store = InMemoryVectorStore::new();
        store.create_collection("col1", 384).await.unwrap();
        store.create_collection("col2", 768).await.unwrap();
        let collections = store.list_collections().await.unwrap();
        assert_eq!(collections.len(), 2);
    }

    #[tokio::test]
    async fn test_cosine_similarity() {
        assert!(
            (InMemoryVectorStore::cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 0.001
        );
        assert!(InMemoryVectorStore::cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 0.001);
        assert!(
            (InMemoryVectorStore::cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs()
                < 0.001
        );
    }

    // ===================== New Tests =====================

    // --- cosine_similarity edge cases ---

    #[test]
    fn test_cosine_similarity_mismatched_lengths_returns_zero() {
        let score = InMemoryVectorStore::cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_cosine_similarity_empty_vectors_returns_zero() {
        let score = InMemoryVectorStore::cosine_similarity(&[], &[]);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_cosine_similarity_first_zero_vector_returns_zero() {
        let score = InMemoryVectorStore::cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_cosine_similarity_second_zero_vector_returns_zero() {
        let score = InMemoryVectorStore::cosine_similarity(&[1.0, 1.0], &[0.0, 0.0]);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_cosine_similarity_high_dimensional() {
        let a: Vec<f32> = (0..384).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..384).map(|i| i as f32).collect();
        let score = InMemoryVectorStore::cosine_similarity(&a, &b);
        assert!((score - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_negative_values() {
        let score = InMemoryVectorStore::cosine_similarity(&[-1.0, -2.0], &[-3.0, -6.0]);
        assert!((score - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_partial_orthogonality() {
        let score = InMemoryVectorStore::cosine_similarity(&[1.0, 1.0, 0.0], &[0.0, 0.0, 1.0]);
        assert!(score.abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_symmetry() {
        let a = [1.0, 2.0, 3.0];
        let b = [4.0, 5.0, 6.0];
        let score_ab = InMemoryVectorStore::cosine_similarity(&a, &b);
        let score_ba = InMemoryVectorStore::cosine_similarity(&b, &a);
        assert!((score_ab - score_ba).abs() < 0.001);
    }

    // --- Default impl ---

    #[tokio::test]
    async fn test_inmemory_default_is_empty() {
        let store = InMemoryVectorStore::default();
        let collections = store.list_collections().await.unwrap();
        assert!(collections.is_empty());
    }

    // --- Serde roundtrips ---

    #[test]
    fn test_vector_store_provider_inmemory_serde_roundtrip() {
        let provider = VectorStoreProvider::InMemory;
        let json = serde_json::to_string(&provider).unwrap();
        let deserialized: VectorStoreProvider = serde_json::from_str(&json).unwrap();
        match deserialized {
            VectorStoreProvider::InMemory => {}
            other => panic!("Expected InMemory, got {:?}", other),
        }
    }

    #[test]
    fn test_vector_store_provider_inmemory_json_value() {
        let provider = VectorStoreProvider::InMemory;
        let json = serde_json::to_string(&provider).unwrap();
        assert_eq!(json, r#"{"provider":"inmemory"}"#);
    }

    #[test]
    fn test_vector_store_provider_deserialize_from_json() {
        let json = r#"{"provider":"inmemory"}"#;
        let provider: VectorStoreProvider = serde_json::from_str(json).unwrap();
        match provider {
            VectorStoreProvider::InMemory => {}
            other => panic!("Expected InMemory, got {:?}", other),
        }
    }

    #[test]
    fn test_collection_stats_serde_roundtrip() {
        let stats = CollectionStats {
            name: "test-col".to_string(),
            document_count: 42,
            dimensions: 768,
            index_size_bytes: Some(1024 * 1024),
            distance_metric: "cosine".to_string(),
        };
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: CollectionStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "test-col");
        assert_eq!(deserialized.document_count, 42);
        assert_eq!(deserialized.dimensions, 768);
        assert_eq!(deserialized.index_size_bytes, Some(1024 * 1024));
        assert_eq!(deserialized.distance_metric, "cosine");
    }

    #[test]
    fn test_collection_stats_none_index_size() {
        let stats = CollectionStats {
            name: "col".to_string(),
            document_count: 0,
            dimensions: 384,
            index_size_bytes: None,
            distance_metric: "euclidean".to_string(),
        };
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: CollectionStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.index_size_bytes, None);
    }

    #[test]
    fn test_collection_info_serde_roundtrip() {
        let info = CollectionInfo {
            name: "docs".to_string(),
            document_count: 100,
            dimensions: 512,
        };
        let json = serde_json::to_string(&info).unwrap();
        let deserialized: CollectionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "docs");
        assert_eq!(deserialized.document_count, 100);
        assert_eq!(deserialized.dimensions, 512);
    }

    // --- VectorStoreProvider::create_store ---

    #[tokio::test]
    async fn test_inmemory_provider_create_store() {
        let provider = VectorStoreProvider::InMemory;
        let store = provider.create_store().await.unwrap();
        assert_eq!(store.provider_name(), "in-memory");
    }

    #[tokio::test]
    async fn test_collection_exists_false_when_missing() {
        let store = InMemoryVectorStore::new();
        assert!(!store.collection_exists("missing").await.unwrap());
    }

    #[tokio::test]
    async fn test_search_with_filters_default_matches_search() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 3).await.unwrap();
        let doc = create_test_document("d1", "x", vec![1.0, 0.0, 0.0]);
        store.upsert("test", &[doc]).await.unwrap();

        let plain = store.search("test", &[1.0, 0.0, 0.0], 5, 0.0).await.unwrap();
        let filtered = store
            .search_with_filters(
                "test",
                &[1.0, 0.0, 0.0],
                5,
                0.0,
                &[("tag".to_string(), "test".to_string())],
            )
            .await
            .unwrap();
        assert_eq!(plain.len(), filtered.len());
        assert_eq!(plain[0].document.id, filtered[0].document.id);
    }

    #[cfg(feature = "pgvector")]
    #[test]
    fn test_pgvector_provider_serde_roundtrip() {
        let provider = VectorStoreProvider::PgVector {
            connection_string: "postgres://localhost/ares".into(),
        };
        let json = serde_json::to_string(&provider).unwrap();
        assert!(json.contains("pgvector"));
        let restored: VectorStoreProvider = serde_json::from_str(&json).unwrap();
        match (provider, restored) {
            (
                VectorStoreProvider::PgVector { connection_string: a },
                VectorStoreProvider::PgVector { connection_string: b },
            ) => assert_eq!(a, b),
            _ => panic!("pgvector roundtrip mismatch"),
        }
    }

    #[cfg(feature = "lancedb")]
    #[test]
    fn test_lancedb_provider_serde_roundtrip() {
        let provider = VectorStoreProvider::LanceDB {
            path: "/data/lancedb".into(),
        };
        let json = serde_json::to_string(&provider).unwrap();
        let restored: VectorStoreProvider = serde_json::from_str(&json).unwrap();
        match (provider, restored) {
            (
                VectorStoreProvider::LanceDB { path: a },
                VectorStoreProvider::LanceDB { path: b },
            ) => assert_eq!(a, b),
            _ => panic!("lancedb roundtrip mismatch"),
        }
    }

    #[cfg(feature = "qdrant")]
    #[test]
    fn test_qdrant_provider_serde_roundtrip() {
        let provider = VectorStoreProvider::Qdrant {
            url: "http://localhost:6334".into(),
            api_key: Some("secret".into()),
        };
        let json = serde_json::to_string(&provider).unwrap();
        let restored: VectorStoreProvider = serde_json::from_str(&json).unwrap();
        match (provider, restored) {
            (
                VectorStoreProvider::Qdrant { url: a, api_key: ka },
                VectorStoreProvider::Qdrant { url: b, api_key: kb },
            ) => {
                assert_eq!(a, b);
                assert_eq!(ka, kb);
            }
            _ => panic!("qdrant roundtrip mismatch"),
        }
    }

    #[cfg(feature = "pinecone")]
    #[test]
    fn test_pinecone_provider_serde_roundtrip() {
        let provider = VectorStoreProvider::Pinecone {
            api_key: "key".into(),
            environment: "us-east-1".into(),
            index_name: "idx".into(),
        };
        let json = serde_json::to_string(&provider).unwrap();
        let restored: VectorStoreProvider = serde_json::from_str(&json).unwrap();
        match (provider, restored) {
            (
                VectorStoreProvider::Pinecone {
                    api_key: a,
                    environment: e,
                    index_name: i,
                },
                VectorStoreProvider::Pinecone {
                    api_key: b,
                    environment: f,
                    index_name: j,
                },
            ) => {
                assert_eq!(a, b);
                assert_eq!(e, f);
                assert_eq!(i, j);
            }
            _ => panic!("pinecone roundtrip mismatch"),
        }
    }

    #[cfg(feature = "pgvector")]
    #[test]
    fn test_from_env_pgvector_url() {
        clear_vector_env();
        std::env::set_var("PGVECTOR_URL", "postgres://localhost/vec");
        let provider = VectorStoreProvider::from_env();
        match provider {
            VectorStoreProvider::PgVector { connection_string } => {
                assert_eq!(connection_string, "postgres://localhost/vec");
            }
            other => panic!("expected PgVector, got {:?}", other),
        }
        std::env::remove_var("PGVECTOR_URL");
    }

    #[cfg(feature = "lancedb")]
    #[test]
    fn test_from_env_lancedb_path() {
        clear_vector_env();
        std::env::set_var("LANCEDB_PATH", "/tmp/lance");
        let provider = VectorStoreProvider::from_env();
        match provider {
            VectorStoreProvider::LanceDB { path } => assert_eq!(path, "/tmp/lance"),
            other => panic!("expected LanceDB, got {:?}", other),
        }
        std::env::remove_var("LANCEDB_PATH");
    }

    #[cfg(feature = "qdrant")]
    #[test]
    fn test_from_env_qdrant_url() {
        clear_vector_env();
        std::env::set_var("QDRANT_URL", "http://qdrant:6334");
        std::env::set_var("QDRANT_API_KEY", "token");
        let provider = VectorStoreProvider::from_env();
        match provider {
            VectorStoreProvider::Qdrant { url, api_key } => {
                assert_eq!(url, "http://qdrant:6334");
                assert_eq!(api_key.as_deref(), Some("token"));
            }
            other => panic!("expected Qdrant, got {:?}", other),
        }
        std::env::remove_var("QDRANT_URL");
        std::env::remove_var("QDRANT_API_KEY");
    }

    #[cfg(feature = "pinecone")]
    #[test]
    fn test_from_env_pinecone_api_key() {
        clear_vector_env();
        std::env::set_var("PINECONE_API_KEY", "pk-test");
        std::env::set_var("PINECONE_ENVIRONMENT", "eu-west-1");
        std::env::set_var("PINECONE_INDEX", "my-index");
        let provider = VectorStoreProvider::from_env();
        match provider {
            VectorStoreProvider::Pinecone {
                api_key,
                environment,
                index_name,
            } => {
                assert_eq!(api_key, "pk-test");
                assert_eq!(environment, "eu-west-1");
                assert_eq!(index_name, "my-index");
            }
            other => panic!("expected Pinecone, got {:?}", other),
        }
        std::env::remove_var("PINECONE_API_KEY");
        std::env::remove_var("PINECONE_ENVIRONMENT");
        std::env::remove_var("PINECONE_INDEX");
    }

    // --- from_env ---

    #[test]
    fn test_from_env_defaults_to_inmemory() {
        clear_vector_env();

        let provider = VectorStoreProvider::from_env();
        match provider {
            #[cfg(not(feature = "ares-vector"))]
            VectorStoreProvider::InMemory => {}
            #[cfg(feature = "ares-vector")]
            VectorStoreProvider::AresVector { path } => {
                assert_eq!(path, None, "AresVector default should have no path");
            }
            other => panic!("Unexpected default provider: {:?}", other),
        }
    }

    // --- InMemoryVectorStore async trait method coverage ---

    #[tokio::test]
    async fn test_upsert_missing_embedding_errors() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 3).await.unwrap();

        let doc_no_embedding = Document {
            id: "bad-doc".to_string(),
            content: "No embedding".to_string(),
            metadata: DocumentMetadata::default(),
            embedding: None,
        };
        let result = store.upsert("test", &[doc_no_embedding]).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AppError::InvalidInput(msg) => assert!(msg.contains("missing embedding")),
            other => panic!("Expected InvalidInput, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_upsert_on_nonexistent_collection_errors() {
        let store = InMemoryVectorStore::new();
        let doc = create_test_document("d1", "content", vec![1.0]);
        let result = store.upsert("nope", &[doc]).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AppError::NotFound(_) => {}
            other => panic!("Expected NotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_search_threshold_filters_results() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 3).await.unwrap();

        let doc1 = create_test_document("d1", "a", vec![1.0, 0.0, 0.0]);
        let doc2 = create_test_document("d2", "b", vec![0.5, 0.866, 0.0]);
        let doc3 = create_test_document("d3", "c", vec![0.0, 1.0, 0.0]);

        store.upsert("test", &[doc1, doc2, doc3]).await.unwrap();

        let results = store
            .search("test", &[1.0, 0.0, 0.0], 10, 0.8)
            .await
            .unwrap();
        assert!(results.iter().all(|r| r.score >= 0.8));
        assert!(!results.iter().any(|r| r.document.id == "d3"));
    }

    #[tokio::test]
    async fn test_search_limit_truncation() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 2).await.unwrap();

        let docs: Vec<Document> = (0..10)
            .map(|i| create_test_document(&format!("d{}", i), "doc", vec![1.0, 0.0]))
            .collect();
        store.upsert("test", &docs).await.unwrap();

        let results = store
            .search("test", &[1.0, 0.0], 3, 0.0)
            .await
            .unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_collection_errors() {
        let store = InMemoryVectorStore::new();
        let result = store.delete_collection("nope").await;
        assert!(result.is_err());
        match result.unwrap_err() {
            AppError::NotFound(_) => {}
            other => panic!("Expected NotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_delete_mixed_existing_and_nonexistent_ids() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 2).await.unwrap();

        let doc = create_test_document("d1", "content", vec![1.0, 0.0]);
        store.upsert("test", &[doc]).await.unwrap();

        let deleted = store
            .delete("test", &["d1".to_string(), "d2".to_string()])
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(store.count("test").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_delete_returns_zero_for_all_nonexistent_ids() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 2).await.unwrap();

        let deleted = store
            .delete("test", &["a".to_string(), "b".to_string()])
            .await
            .unwrap();
        assert_eq!(deleted, 0);
    }

    #[tokio::test]
    async fn test_collection_stats_after_operations() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 4).await.unwrap();

        let doc = create_test_document("d1", "hello", vec![1.0, 0.0, 0.0, 0.0]);
        store.upsert("test", &[doc]).await.unwrap();

        let stats = store.collection_stats("test").await.unwrap();
        assert_eq!(stats.document_count, 1);
        assert_eq!(stats.dimensions, 4);
        assert_eq!(stats.distance_metric, "cosine");
        assert_eq!(stats.index_size_bytes, None);

        store.delete("test", &["d1".to_string()]).await.unwrap();
        let stats = store.collection_stats("test").await.unwrap();
        assert_eq!(stats.document_count, 0);
    }

    #[tokio::test]
    async fn test_collection_stats_nonexistent_errors() {
        let store = InMemoryVectorStore::new();
        let result = store.collection_stats("nope").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_collections_after_delete() {
        let store = InMemoryVectorStore::new();
        store.create_collection("a", 128).await.unwrap();
        store.create_collection("b", 256).await.unwrap();
        store.delete_collection("a").await.unwrap();

        let cols = store.list_collections().await.unwrap();
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].name, "b");
    }

    #[tokio::test]
    async fn test_get_nonexistent_collection_errors() {
        let store = InMemoryVectorStore::new();
        let result = store.get("nope", "id").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_provider_name() {
        let store = InMemoryVectorStore::new();
        assert_eq!(store.provider_name(), "in-memory");
    }

    #[tokio::test]
    async fn test_search_empty_collection_returns_empty() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 3).await.unwrap();

        let results = store
            .search("test", &[1.0, 0.0, 0.0], 10, 0.0)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_search_results_excludes_embeddings() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 3).await.unwrap();

        let doc = create_test_document("d1", "content", vec![1.0, 0.0, 0.0]);
        store.upsert("test", &[doc]).await.unwrap();

        let results = store
            .search("test", &[1.0, 0.0, 0.0], 10, 0.0)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].document.embedding.is_none());
    }

    #[tokio::test]
    async fn test_upsert_updates_existing_document() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 3).await.unwrap();

        let doc1 = create_test_document("d1", "original", vec![1.0, 0.0, 0.0]);
        store.upsert("test", &[doc1]).await.unwrap();

        let doc1_updated = create_test_document("d1", "updated", vec![0.0, 1.0, 0.0]);
        store.upsert("test", &[doc1_updated]).await.unwrap();

        let retrieved = store.get("test", "d1").await.unwrap().unwrap();
        assert_eq!(retrieved.content, "updated");
        assert_eq!(store.count("test").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_search_score_ordering_descending() {
        let store = InMemoryVectorStore::new();
        store.create_collection("test", 3).await.unwrap();

        let doc1 = create_test_document("d1", "far", vec![0.0, 0.0, 1.0]);
        let doc2 = create_test_document("d2", "near", vec![0.9, 0.1, 0.0]);
        let doc3 = create_test_document("d3", "exact", vec![1.0, 0.0, 0.0]);

        store.upsert("test", &[doc1, doc2, doc3]).await.unwrap();

        let results = store
            .search("test", &[1.0, 0.0, 0.0], 10, 0.0)
            .await
            .unwrap();

        for i in 1..results.len() {
            assert!(results[i - 1].score >= results[i].score);
        }
        assert_eq!(results[0].document.id, "d3");
    }
}
