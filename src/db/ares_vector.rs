//! AresVector - Pure Rust Vector Store Implementation
//!
//! This module provides a high-performance, pure-Rust vector store
//! using the HNSW (Hierarchical Navigable Small World) algorithm.
//!
//! # Features
//!
//! - **No native dependencies**: Compiles on any platform Rust supports
//! - **Embedded**: No separate server process required
//! - **Persistent**: Optional disk persistence with efficient serialization
//! - **Thread-safe**: Lock-free concurrent reads, synchronized writes
//!
//! # Example
//!
//! ```rust,ignore
//! let store = AresVectorStore::new(Some("./data/vectors".into())).await?;
//! store.create_collection("documents", 384).await?;
//! store.upsert("documents", &docs).await?;
//! let results = store.search("documents", &embedding, 10, 0.5).await?;
//! ```

use crate::types::{AppError, Document, Result, SearchResult};
use async_trait::async_trait;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::vectorstore::{CollectionInfo, CollectionStats, VectorStore};
use ares_vector::{Config, DistanceMetric, VectorDb, VectorMetadata};

// ============================================================================
// AresVector Store Implementation
// ============================================================================

/// Pure Rust vector store using HNSW algorithm.
///
/// This is the default vector store for Ares, providing:
/// - Zero external dependencies (no protobuf, no GRPC)
/// - Embedded operation (no separate server)
/// - Optional persistence to disk
/// - High-performance approximate nearest neighbor search
pub struct AresVectorStore {
    /// The underlying vector database (VectorDb is Clone and uses Arc internally)
    db: VectorDb,
    /// Storage path (None for in-memory)
    path: Option<PathBuf>,
    /// Document storage (for full document retrieval)
    documents: Arc<RwLock<HashMap<String, HashMap<String, Document>>>>,
}

impl AresVectorStore {
    /// Create a new AresVector store.
    ///
    /// # Arguments
    ///
    /// * `path` - Optional path to persist data. If None, operates in-memory.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be initialized or loaded.
    pub async fn new(path: Option<String>) -> Result<Self> {
        let path_buf = path.map(PathBuf::from);

        // Configure the vector database
        let config = if let Some(ref p) = path_buf {
            Config::persistent(p.to_string_lossy().to_string())
        } else {
            Config::memory()
        };

        // Create or load the database
        let db = VectorDb::open(config).await.map_err(|e| {
            AppError::Configuration(format!("Failed to initialize AresVector: {}", e))
        })?;

        // Load existing collections if persistent
        let store = Self {
            db,
            path: path_buf,
            documents: Arc::new(RwLock::new(HashMap::new())),
        };

        // If persistent, try to load document metadata
        if let Some(ref path) = store.path {
            store.load_documents(path).await?;
        }

        Ok(store)
    }

    /// Load document metadata from disk.
    async fn load_documents(&self, path: &Path) -> Result<()> {
        let docs_path = path.join("documents.json");
        if docs_path.exists() {
            let data = tokio::fs::read_to_string(&docs_path).await.map_err(|e| {
                AppError::Configuration(format!("Failed to read documents file: {}", e))
            })?;

            let loaded: HashMap<String, HashMap<String, Document>> = serde_json::from_str(&data)
                .map_err(|e| {
                    AppError::Configuration(format!("Failed to parse documents file: {}", e))
                })?;

            let mut docs = self.documents.write();
            *docs = loaded;
        }
        Ok(())
    }

    /// Save document metadata to disk.
    async fn save_documents(&self) -> Result<()> {
        if let Some(ref path) = self.path {
            // Clone the data to avoid holding lock across await
            let data = {
                let docs = self.documents.read();
                serde_json::to_string_pretty(&*docs).map_err(|e| {
                    AppError::Internal(format!("Failed to serialize documents: {}", e))
                })?
            };

            // Ensure directory exists
            tokio::fs::create_dir_all(path).await.map_err(|e| {
                AppError::Internal(format!("Failed to create data directory: {}", e))
            })?;

            let docs_path = path.join("documents.json");
            tokio::fs::write(&docs_path, data).await.map_err(|e| {
                AppError::Internal(format!("Failed to write documents file: {}", e))
            })?;
        }
        Ok(())
    }
}

#[async_trait]
impl VectorStore for AresVectorStore {
    fn provider_name(&self) -> &'static str {
        "ares-vector"
    }

    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        // Check if collection already exists
        if self.db.list_collections().contains(&name.to_string()) {
            return Err(AppError::Configuration(format!(
                "Collection '{}' already exists",
                name
            )));
        }

        // Create the collection with default configuration
        self.db
            .create_collection(name, dimensions, DistanceMetric::Cosine)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to create collection: {}", e)))?;

        // Initialize document storage for this collection
        {
            let mut docs = self.documents.write();
            docs.insert(name.to_string(), HashMap::new());
        }

        // Persist if configured
        if self.path.is_some() {
            self.save_documents().await?;
        }

        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        self.db
            .delete_collection(name)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to delete collection: {}", e)))?;

        // Remove document storage
        {
            let mut docs = self.documents.write();
            docs.remove(name);
        }

        // Persist if configured
        if self.path.is_some() {
            self.save_documents().await?;
        }

        Ok(())
    }

    async fn list_collections(&self) -> Result<Vec<CollectionInfo>> {
        let collections = self.db.list_collections();

        let mut infos = Vec::with_capacity(collections.len());
        for name in collections {
            if let Ok(collection) = self.db.get_collection(&name) {
                let stats = collection.stats();
                infos.push(CollectionInfo {
                    name,
                    dimensions: stats.dimensions,
                    document_count: stats.vector_count,
                });
            }
        }

        Ok(infos)
    }

    async fn collection_exists(&self, name: &str) -> Result<bool> {
        Ok(self.db.list_collections().contains(&name.to_string()))
    }

    async fn collection_stats(&self, name: &str) -> Result<CollectionStats> {
        let collection = self
            .db
            .get_collection(name)
            .map_err(|_| AppError::NotFound(format!("Collection '{}' not found", name)))?;

        let stats = collection.stats();

        Ok(CollectionStats {
            name: stats.name,
            document_count: stats.vector_count,
            dimensions: stats.dimensions,
            index_size_bytes: Some(stats.memory_bytes as u64),
            distance_metric: format!("{:?}", stats.metric),
        })
    }

    async fn upsert(&self, collection: &str, documents: &[Document]) -> Result<usize> {
        if documents.is_empty() {
            return Ok(0);
        }

        // Get or verify collection exists
        if !self.db.list_collections().contains(&collection.to_string()) {
            return Err(AppError::NotFound(format!(
                "Collection '{}' not found",
                collection
            )));
        }

        let mut upserted = 0;

        for doc in documents {
            let embedding = doc.embedding.as_ref().ok_or_else(|| {
                AppError::Internal(format!("Document '{}' missing embedding", doc.id))
            })?;

            // Convert document metadata to vector metadata
            let meta = VectorMetadata::from_pairs([
                (
                    "title",
                    ares_vector::types::MetadataValue::String(doc.metadata.title.clone()),
                ),
                (
                    "source",
                    ares_vector::types::MetadataValue::String(doc.metadata.source.clone()),
                ),
            ]);

            // Insert/update in vector index
            self.db
                .insert(collection, &doc.id, embedding, Some(meta))
                .await
                .map_err(|e| AppError::Internal(format!("Failed to insert vector: {}", e)))?;

            // Store full document
            {
                let mut docs = self.documents.write();
                let collection_docs = docs.entry(collection.to_string()).or_default();
                collection_docs.insert(doc.id.clone(), doc.clone());
            }

            upserted += 1;
        }

        // Persist if configured
        if self.path.is_some() {
            self.save_documents().await?;
        }

        Ok(upserted)
    }

    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>> {
        // Search in vector index
        let vector_results = self
            .db
            .search(collection, embedding, limit * 2) // Fetch extra for threshold filtering
            .await
            .map_err(|e| AppError::Internal(format!("Search failed: {}", e)))?;

        // Get full documents and filter by threshold
        let docs = self.documents.read();
        let collection_docs = docs
            .get(collection)
            .ok_or_else(|| AppError::NotFound(format!("Collection '{}' not found", collection)))?;

        let mut results = Vec::with_capacity(limit);
        for result in vector_results {
            // Use score directly (already converted from distance)
            let similarity = result.score;

            if similarity >= threshold {
                if let Some(doc) = collection_docs.get(&result.id) {
                    results.push(SearchResult {
                        document: doc.clone(),
                        score: similarity,
                    });

                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }

        // Sort by score descending
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(results)
    }

    async fn delete(&self, collection: &str, ids: &[String]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }

        let mut deleted = 0;

        for id in ids {
            if let Ok(true) = self.db.delete(collection, id).await {
                // Remove from document storage
                let mut docs = self.documents.write();
                if let Some(collection_docs) = docs.get_mut(collection) {
                    if collection_docs.remove(id).is_some() {
                        deleted += 1;
                    }
                }
            }
        }

        // Persist if configured
        if self.path.is_some() {
            self.save_documents().await?;
        }

        Ok(deleted)
    }

    async fn get(&self, collection: &str, id: &str) -> Result<Option<Document>> {
        let docs = self.documents.read();

        let collection_docs = docs
            .get(collection)
            .ok_or_else(|| AppError::NotFound(format!("Collection '{}' not found", collection)))?;

        Ok(collection_docs.get(id).cloned())
    }
}

impl Default for AresVectorStore {
    fn default() -> Self {
        // Create an in-memory store synchronously for default
        // Note: This requires a tokio runtime to be available
        let config = Config::memory();
        let db = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                VectorDb::open(config)
                    .await
                    .expect("Failed to create in-memory VectorDb")
            })
        });

        Self {
            db,
            path: None,
            documents: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DocumentMetadata;
    use chrono::Utc;

    #[tokio::test]
    async fn test_create_and_search() {
        let store = AresVectorStore::new(None).await.unwrap();

        // Create collection
        store.create_collection("test", 3).await.unwrap();

        // Create test documents
        let docs = vec![
            Document {
                id: "doc1".to_string(),
                content: "Hello world".to_string(),
                metadata: DocumentMetadata {
                    title: "Test 1".to_string(),
                    source: "test".to_string(),
                    created_at: Utc::now(),
                    tags: vec![],
                },
                embedding: Some(vec![1.0, 0.0, 0.0]),
            },
            Document {
                id: "doc2".to_string(),
                content: "Goodbye world".to_string(),
                metadata: DocumentMetadata {
                    title: "Test 2".to_string(),
                    source: "test".to_string(),
                    created_at: Utc::now(),
                    tags: vec![],
                },
                embedding: Some(vec![0.0, 1.0, 0.0]),
            },
        ];

        // Upsert
        let count = store.upsert("test", &docs).await.unwrap();
        assert_eq!(count, 2);

        // Search
        let query = vec![1.0, 0.1, 0.0]; // Close to doc1
        let results = store.search("test", &query, 10, 0.0).await.unwrap();

        assert!(!results.is_empty());
        assert_eq!(results[0].document.id, "doc1");
    }

    #[tokio::test]
    async fn test_collection_operations() {
        let store = AresVectorStore::new(None).await.unwrap();

        // Create
        store.create_collection("col1", 128).await.unwrap();
        store.create_collection("col2", 256).await.unwrap();

        // List
        let collections = store.list_collections().await.unwrap();
        assert_eq!(collections.len(), 2);

        // Exists
        assert!(store.collection_exists("col1").await.unwrap());
        assert!(!store.collection_exists("col3").await.unwrap());

        // Delete
        store.delete_collection("col1").await.unwrap();
        assert!(!store.collection_exists("col1").await.unwrap());
    }
}
