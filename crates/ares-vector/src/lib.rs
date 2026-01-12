//! # ares-vector
//!
//! A pure-Rust embedded vector database with HNSW (Hierarchical Navigable Small World)
//! indexing for high-performance approximate nearest neighbor search.
//!
//! ## Features
//!
//! - **Pure Rust**: No native dependencies, compiles anywhere Rust does
//! - **HNSW Indexing**: Fast approximate nearest neighbor search
//! - **Thread-Safe**: Designed for concurrent read/write access
//! - **Persistence**: Optional disk-based storage with memory-mapped files
//! - **Multiple Distance Metrics**: Cosine, Euclidean (L2), Dot Product, Manhattan (L1)
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use ares_vector::{VectorDb, Config, DistanceMetric};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), ares_vector::Error> {
//!     // Create an in-memory database
//!     let db = VectorDb::open(Config::memory()).await?;
//!     
//!     // Create a collection for 384-dimensional vectors
//!     db.create_collection("documents", 384, DistanceMetric::Cosine).await?;
//!     
//!     // Insert vectors
//!     let embedding = vec![0.1f32; 384];
//!     db.insert("documents", "doc1", &embedding, None).await?;
//!     
//!     // Search for similar vectors
//!     let query = vec![0.1f32; 384];
//!     let results = db.search("documents", &query, 10).await?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                         VectorDb                             │
//! │  ┌─────────────────────────────────────────────────────────┐│
//! │  │                    Collection                            ││
//! │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐ ││
//! │  │  │ HNSW Index  │  │  ID Mapping │  │ Metadata Store  │ ││
//! │  │  │  (search)   │  │ (str → u64) │  │  (optional)     │ ││
//! │  │  └─────────────┘  └─────────────┘  └─────────────────┘ ││
//! │  └─────────────────────────────────────────────────────────┘│
//! │                                                              │
//! │  ┌─────────────────────────────────────────────────────────┐│
//! │  │               Persistence Layer (optional)               ││
//! │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐ ││
//! │  │  │   Bincode   │  │  Snapshots  │  │  WAL (future)   │ ││
//! │  │  └─────────────┘  └─────────────┘  └─────────────────┘ ││
//! │  └─────────────────────────────────────────────────────────┘│
//! └─────────────────────────────────────────────────────────────┘
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod collection;
pub mod config;
pub mod distance;
pub mod error;
pub mod index;
pub mod persistence;
pub mod types;

// Re-exports for convenience
pub use collection::Collection;
pub use config::Config;
pub use distance::DistanceMetric;
pub use error::{Error, Result};
pub use types::{SearchResult, VectorId, VectorMetadata};

use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, instrument, warn};

/// The main vector database instance.
///
/// `VectorDb` manages multiple collections, each containing vectors of a specific
/// dimensionality. It provides thread-safe access to all operations.
///
/// # Thread Safety
///
/// All operations on `VectorDb` are thread-safe. Uses `scc::HashMap` for
/// lock-free concurrent access that is safe across `.await` points.
/// Multiple readers can access the database concurrently, and write
/// operations use fine-grained locking per entry.
#[derive(Clone)]
pub struct VectorDb {
    inner: Arc<VectorDbInner>,
}

struct VectorDbInner {
    config: Config,
    /// Async-safe concurrent hashmap from scc crate
    collections: scc::HashMap<String, Arc<Collection>>,
}

impl VectorDb {
    /// Open or create a vector database with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Database configuration (memory or persistent).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // In-memory database
    /// let db = VectorDb::open(Config::memory()).await?;
    ///
    /// // Persistent database
    /// let db = VectorDb::open(Config::persistent("./data/vectors")).await?;
    /// ```
    #[instrument(skip(config), fields(persistent = config.data_path.is_some()))]
    pub async fn open(config: Config) -> Result<Self> {
        info!("Opening vector database");

        let db = Self {
            inner: Arc::new(VectorDbInner {
                config: config.clone(),
                collections: scc::HashMap::new(),
            }),
        };

        // Load existing collections from disk if persistent
        if let Some(ref path) = config.data_path {
            db.load_collections(path).await?;
        }

        Ok(db)
    }

    /// Create a new collection with the specified parameters.
    ///
    /// # Arguments
    ///
    /// * `name` - Unique name for the collection.
    /// * `dimensions` - Dimensionality of vectors (e.g., 384 for BGE-small).
    /// * `metric` - Distance metric to use for similarity calculations.
    ///
    /// # Errors
    ///
    /// Returns an error if a collection with the same name already exists.
    #[instrument(skip(self))]
    pub async fn create_collection(
        &self,
        name: &str,
        dimensions: usize,
        metric: DistanceMetric,
    ) -> Result<()> {
        info!(name, dimensions, ?metric, "Creating collection");

        // Check if collection already exists (lock-free read)
        if self.inner.collections.contains(name) {
            return Err(Error::CollectionExists(name.to_string()));
        }

        let collection = Collection::new(
            name.to_string(),
            dimensions,
            metric,
            self.inner.config.hnsw_config.clone(),
        )?;

        // Insert returns Err if key already exists (handles race condition)
        if self.inner.collections.insert(name.to_string(), Arc::new(collection)).is_err() {
            return Err(Error::CollectionExists(name.to_string()));
        }

        // Persist if configured
        if let Some(ref path) = self.inner.config.data_path {
            self.persist_collection_metadata(path, name).await?;
        }

        Ok(())
    }

    /// Delete a collection and all its data.
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the collection to delete.
    ///
    /// # Errors
    ///
    /// Returns an error if the collection doesn't exist.
    #[instrument(skip(self))]
    pub async fn delete_collection(&self, name: &str) -> Result<()> {
        info!(name, "Deleting collection");

        // remove returns Option<(K, V)>
        if self.inner.collections.remove(name).is_none() {
            return Err(Error::CollectionNotFound(name.to_string()));
        }

        // Remove from disk if persistent
        if let Some(ref path) = self.inner.config.data_path {
            self.delete_collection_files(path, name).await?;
        }

        Ok(())
    }

    /// Check if a collection exists.
    pub fn collection_exists(&self, name: &str) -> bool {
        self.inner.collections.contains(name)
    }

    /// List all collection names.
    pub fn list_collections(&self) -> Vec<String> {
        let mut names = Vec::new();
        self.inner.collections.scan(|k, _| {
            names.push(k.clone());
        });
        names
    }

    /// Get a reference to a collection.
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the collection.
    ///
    /// # Errors
    ///
    /// Returns an error if the collection doesn't exist.
    pub fn get_collection(&self, name: &str) -> Result<Arc<Collection>> {
        self.inner
            .collections
            .read(name, |_, v| v.clone())
            .ok_or_else(|| Error::CollectionNotFound(name.to_string()))
    }

    /// Insert a vector into a collection.
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the collection.
    /// * `id` - Unique string identifier for the vector.
    /// * `vector` - The embedding vector to insert.
    /// * `metadata` - Optional metadata to associate with the vector.
    ///
    /// # Errors
    ///
    /// Returns an error if the collection doesn't exist or the vector
    /// dimensions don't match the collection.
    #[instrument(skip(self, vector, metadata), fields(collection, id, dim = vector.len()))]
    pub async fn insert(
        &self,
        collection: &str,
        id: &str,
        vector: &[f32],
        metadata: Option<VectorMetadata>,
    ) -> Result<()> {
        let col = self.get_collection(collection)?;
        col.insert(id, vector, metadata)?;
        debug!("Inserted vector");
        Ok(())
    }

    /// Insert multiple vectors into a collection.
    ///
    /// This is more efficient than calling `insert` repeatedly as it batches
    /// the index updates.
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the collection.
    /// * `vectors` - Iterator of (id, vector, metadata) tuples.
    ///
    /// # Returns
    ///
    /// The number of vectors successfully inserted.
    #[instrument(skip(self, vectors), fields(collection))]
    pub async fn insert_batch<'a, I>(&self, collection: &str, vectors: I) -> Result<usize>
    where
        I: IntoIterator<Item = (&'a str, &'a [f32], Option<VectorMetadata>)>,
    {
        let col = self.get_collection(collection)?;
        let count = col.insert_batch(vectors)?;
        debug!(count, "Inserted batch");
        Ok(count)
    }

    /// Update a vector in a collection.
    ///
    /// This is equivalent to delete + insert but may be more efficient
    /// for some index implementations.
    #[instrument(skip(self, vector, metadata), fields(collection, id))]
    pub async fn update(
        &self,
        collection: &str,
        id: &str,
        vector: &[f32],
        metadata: Option<VectorMetadata>,
    ) -> Result<()> {
        let col = self.get_collection(collection)?;
        col.update(id, vector, metadata)?;
        Ok(())
    }

    /// Delete a vector from a collection.
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the collection.
    /// * `id` - ID of the vector to delete.
    ///
    /// # Returns
    ///
    /// `true` if the vector was found and deleted, `false` if it didn't exist.
    #[instrument(skip(self), fields(collection, id))]
    pub async fn delete(&self, collection: &str, id: &str) -> Result<bool> {
        let col = self.get_collection(collection)?;
        let deleted = col.delete(id)?;
        debug!(deleted, "Delete result");
        Ok(deleted)
    }

    /// Delete multiple vectors from a collection.
    ///
    /// # Returns
    ///
    /// The number of vectors actually deleted.
    #[instrument(skip(self, ids), fields(collection, count = ids.len()))]
    pub async fn delete_batch(&self, collection: &str, ids: &[&str]) -> Result<usize> {
        let col = self.get_collection(collection)?;
        let count = col.delete_batch(ids)?;
        debug!(count, "Deleted batch");
        Ok(count)
    }

    /// Search for similar vectors.
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the collection to search.
    /// * `query` - Query vector to find similar vectors to.
    /// * `limit` - Maximum number of results to return.
    ///
    /// # Returns
    ///
    /// A vector of search results, sorted by similarity (best first).
    #[instrument(skip(self, query), fields(collection, limit, dim = query.len()))]
    pub async fn search(
        &self,
        collection: &str,
        query: &[f32],
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let col = self.get_collection(collection)?;
        let results = col.search(query, limit)?;
        debug!(count = results.len(), "Search completed");
        Ok(results)
    }

    /// Search with a minimum score threshold.
    ///
    /// # Arguments
    ///
    /// * `collection` - Name of the collection.
    /// * `query` - Query vector.
    /// * `limit` - Maximum results.
    /// * `min_score` - Minimum similarity score (0.0 to 1.0 for cosine).
    #[instrument(skip(self, query), fields(collection, limit, min_score))]
    pub async fn search_with_threshold(
        &self,
        collection: &str,
        query: &[f32],
        limit: usize,
        min_score: f32,
    ) -> Result<Vec<SearchResult>> {
        let col = self.get_collection(collection)?;
        let results = col.search_with_threshold(query, limit, min_score)?;
        Ok(results)
    }

    /// Get a vector by ID.
    ///
    /// # Returns
    ///
    /// The vector and its metadata if found.
    pub async fn get(&self, collection: &str, id: &str) -> Result<Option<(Vec<f32>, Option<VectorMetadata>)>> {
        let col = self.get_collection(collection)?;
        Ok(col.get(id))
    }

    /// Check if a vector exists.
    pub fn contains(&self, collection: &str, id: &str) -> Result<bool> {
        let col = self.get_collection(collection)?;
        Ok(col.contains(id))
    }

    /// Get the number of vectors in a collection.
    pub fn count(&self, collection: &str) -> Result<usize> {
        let col = self.get_collection(collection)?;
        Ok(col.len())
    }

    /// Get collection statistics.
    pub fn collection_stats(&self, collection: &str) -> Result<CollectionStats> {
        let col = self.get_collection(collection)?;
        Ok(col.stats())
    }

    /// Persist the current state to disk.
    ///
    /// This is only relevant for persistent databases. For in-memory databases,
    /// this is a no-op.
    #[instrument(skip(self))]
    pub async fn persist(&self) -> Result<()> {
        let Some(ref path) = self.inner.config.data_path else {
            debug!("Skipping persist for in-memory database");
            return Ok(());
        };

        info!("Persisting database to disk");
        
        // Collect names and collections for persistence
        let mut to_persist: Vec<(String, Arc<Collection>)> = Vec::new();
        self.inner.collections.scan(|name, collection| {
            to_persist.push((name.clone(), collection.clone()));
        });

        for (name, collection) in to_persist {
            self.persist_collection(path, &name, &collection).await?;
        }

        Ok(())
    }

    /// Force a compaction of the HNSW indices.
    ///
    /// This can reclaim space after many deletions.
    #[instrument(skip(self))]
    pub async fn compact(&self, collection: &str) -> Result<()> {
        let col = self.get_collection(collection)?;
        col.compact()?;
        Ok(())
    }

    // Internal: Load collections from disk
    async fn load_collections(&self, path: &PathBuf) -> Result<()> {
        if !path.exists() {
            tokio::fs::create_dir_all(path).await?;
            return Ok(());
        }

        // Load collection metadata and indices
        let metadata_path = path.join("collections.json");
        if !metadata_path.exists() {
            return Ok(());
        }

        let data = tokio::fs::read_to_string(&metadata_path).await?;
        let collection_names: Vec<String> = serde_json::from_str(&data)
            .map_err(|e| Error::Persistence(format!("Failed to parse collections.json: {}", e)))?;

        for name in collection_names {
            match self.load_collection(path, &name).await {
                Ok(collection) => {
                    // scc::HashMap::insert returns Err if key exists, Ok otherwise
                    let _ = self.inner.collections.insert(name.clone(), Arc::new(collection));
                    info!(name, "Loaded collection");
                }
                Err(e) => {
                    warn!(name, error = %e, "Failed to load collection, skipping");
                }
            }
        }

        Ok(())
    }

    async fn load_collection(&self, base_path: &PathBuf, name: &str) -> Result<Collection> {
        persistence::load_collection(base_path, name).await
    }

    async fn persist_collection(
        &self,
        base_path: &PathBuf,
        name: &str,
        collection: &Collection,
    ) -> Result<()> {
        persistence::save_collection(base_path, name, collection).await
    }

    async fn persist_collection_metadata(&self, base_path: &PathBuf, _name: &str) -> Result<()> {
        let collections = self.list_collections();
        let metadata_path = base_path.join("collections.json");
        let data = serde_json::to_string_pretty(&collections)
            .map_err(|e| Error::Persistence(format!("Failed to serialize collections: {}", e)))?;
        tokio::fs::write(&metadata_path, data).await?;
        Ok(())
    }

    async fn delete_collection_files(&self, base_path: &PathBuf, name: &str) -> Result<()> {
        let collection_path = base_path.join(name);
        if collection_path.exists() {
            tokio::fs::remove_dir_all(&collection_path).await?;
        }
        // Update metadata
        self.persist_collection_metadata(base_path, name).await?;
        Ok(())
    }
}

/// Statistics about a collection.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CollectionStats {
    /// Name of the collection.
    pub name: String,
    /// Number of vectors in the collection.
    pub vector_count: usize,
    /// Dimensionality of vectors.
    pub dimensions: usize,
    /// Distance metric used.
    pub metric: DistanceMetric,
    /// Approximate memory usage in bytes.
    pub memory_bytes: usize,
    /// HNSW index parameters.
    pub hnsw_params: HnswParams,
}

/// HNSW index parameters.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HnswParams {
    /// Maximum number of connections per layer.
    pub m: usize,
    /// Size of the dynamic candidate list during construction.
    pub ef_construction: usize,
    /// Size of the dynamic candidate list during search.
    pub ef_search: usize,
}

// Required for serde_json usage
use serde_json;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_and_search() {
        let db = VectorDb::open(Config::memory()).await.unwrap();

        db.create_collection("test", 3, DistanceMetric::Cosine)
            .await
            .unwrap();

        db.insert("test", "vec1", &[1.0, 0.0, 0.0], None)
            .await
            .unwrap();
        db.insert("test", "vec2", &[0.0, 1.0, 0.0], None)
            .await
            .unwrap();
        db.insert("test", "vec3", &[0.9, 0.1, 0.0], None)
            .await
            .unwrap();

        let results = db.search("test", &[1.0, 0.0, 0.0], 10).await.unwrap();

        assert!(!results.is_empty());
        assert_eq!(results[0].id, "vec1");
    }

    #[tokio::test]
    async fn test_collection_lifecycle() {
        let db = VectorDb::open(Config::memory()).await.unwrap();

        assert!(!db.collection_exists("test"));

        db.create_collection("test", 128, DistanceMetric::Euclidean)
            .await
            .unwrap();
        assert!(db.collection_exists("test"));

        db.delete_collection("test").await.unwrap();
        assert!(!db.collection_exists("test"));
    }

    #[tokio::test]
    async fn test_duplicate_collection_error() {
        let db = VectorDb::open(Config::memory()).await.unwrap();

        db.create_collection("test", 128, DistanceMetric::Cosine)
            .await
            .unwrap();

        let result = db
            .create_collection("test", 128, DistanceMetric::Cosine)
            .await;
        assert!(matches!(result, Err(Error::CollectionExists(_))));
    }
}
