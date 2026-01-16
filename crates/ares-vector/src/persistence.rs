//! Persistence layer for ares-vector.
//!
//! This module handles saving and loading collections to/from disk.

use crate::collection::Collection;
use crate::config::HnswConfig;
use crate::distance::DistanceMetric;
use crate::error::{Error, Result};
use crate::types::VectorMetadata;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{debug, info, warn};

/// Collection metadata stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CollectionMetadata {
    name: String,
    dimensions: usize,
    metric: String,
    hnsw_m: usize,
    hnsw_ef_construction: usize,
    hnsw_ef_search: usize,
}

/// Stored vector data for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredVectorData {
    id: String,
    vector: Vec<f32>,
    metadata: Option<VectorMetadata>,
}

/// Save a collection to disk.
///
/// Creates the following files:
/// - `{base_path}/{name}/metadata.json` - Collection metadata
/// - `{base_path}/{name}/vectors.json` - Vector data (JSON format)
pub async fn save_collection(base_path: &Path, name: &str, collection: &Collection) -> Result<()> {
    let collection_path = base_path.join(name);
    tokio::fs::create_dir_all(&collection_path).await?;

    // Save metadata
    let metadata = CollectionMetadata {
        name: name.to_string(),
        dimensions: collection.dimensions(),
        metric: collection.metric().name().to_string(),
        hnsw_m: collection.hnsw_config().m,
        hnsw_ef_construction: collection.hnsw_config().ef_construction,
        hnsw_ef_search: collection.hnsw_config().ef_search,
    };

    let metadata_path = collection_path.join("metadata.json");
    let metadata_json = serde_json::to_string_pretty(&metadata)
        .map_err(|e| Error::Persistence(format!("Failed to serialize metadata: {}", e)))?;
    tokio::fs::write(&metadata_path, metadata_json).await?;

    // Export all vectors from the collection
    let exported = collection.export_all();
    let vectors: Vec<StoredVectorData> = exported
        .into_iter()
        .map(|(id, vector, metadata)| StoredVectorData {
            id,
            vector,
            metadata,
        })
        .collect();

    let vectors_path = collection_path.join("vectors.json");
    let vectors_json = serde_json::to_string(&vectors)
        .map_err(|e| Error::Persistence(format!("Failed to serialize vectors: {}", e)))?;
    tokio::fs::write(&vectors_path, vectors_json).await?;

    info!(name, vectors = vectors.len(), path = ?collection_path, "Saved collection");
    Ok(())
}

/// Load a collection from disk.
pub async fn load_collection(base_path: &Path, name: &str) -> Result<Collection> {
    let collection_path = base_path.join(name);

    if !collection_path.exists() {
        return Err(Error::CollectionNotFound(name.to_string()));
    }

    // Load metadata
    let metadata_path = collection_path.join("metadata.json");
    let metadata_json = tokio::fs::read_to_string(&metadata_path).await?;
    let metadata: CollectionMetadata = serde_json::from_str(&metadata_json)
        .map_err(|e| Error::Persistence(format!("Failed to parse metadata: {}", e)))?;

    // Parse distance metric
    let metric: DistanceMetric = metadata
        .metric
        .parse()
        .map_err(|e: String| Error::Persistence(e))?;

    // Create HNSW config
    let hnsw_config = HnswConfig {
        m: metadata.hnsw_m,
        m_max: metadata.hnsw_m * 2,
        ef_construction: metadata.hnsw_ef_construction,
        ef_search: metadata.hnsw_ef_search,
        parallel_construction: true,
        num_threads: 0,
    };

    // Create collection
    let collection = Collection::new(
        metadata.name.clone(),
        metadata.dimensions,
        metric,
        hnsw_config,
    )?;

    // Load vectors
    let vectors_path = collection_path.join("vectors.json");
    if vectors_path.exists() {
        let vectors_json = tokio::fs::read_to_string(&vectors_path).await?;
        let vectors: Vec<StoredVectorData> = serde_json::from_str(&vectors_json)
            .map_err(|e| Error::Persistence(format!("Failed to parse vectors: {}", e)))?;

        let count = vectors.len();
        for stored in vectors {
            if let Err(e) = collection.insert(&stored.id, &stored.vector, stored.metadata) {
                warn!(id = stored.id, error = %e, "Failed to load vector");
            }
        }

        debug!(name, count, "Loaded vectors");
    }

    info!(name, dimensions = metadata.dimensions, "Loaded collection");
    Ok(collection)
}

/// Enhanced persistence with bincode (when serde feature is enabled).
#[cfg(feature = "serde")]
#[allow(dead_code)]
pub(crate) mod bincode_persistence {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Save vectors using bincode for efficiency.
    pub(crate) async fn save_vectors_bincode(
        path: &Path,
        vectors: &[StoredVectorData],
    ) -> Result<()> {
        let data = bincode::serialize(vectors)
            .map_err(|e| Error::Persistence(format!("Bincode serialize error: {}", e)))?;

        let mut file = tokio::fs::File::create(path).await?;
        file.write_all(&data).await?;
        file.flush().await?;

        Ok(())
    }

    /// Load vectors using bincode.
    pub(crate) async fn load_vectors_bincode(path: &Path) -> Result<Vec<StoredVectorData>> {
        let mut file = tokio::fs::File::open(path).await?;
        let mut data = Vec::new();
        file.read_to_end(&mut data).await?;

        let vectors: Vec<StoredVectorData> = bincode::deserialize(&data)
            .map_err(|e| Error::Persistence(format!("Bincode deserialize error: {}", e)))?;

        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_save_load_collection() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create and save collection
        let collection = Collection::new(
            "test".to_string(),
            3,
            DistanceMetric::Cosine,
            HnswConfig::default(),
        )
        .unwrap();

        collection.insert("vec1", &[1.0, 0.0, 0.0], None).unwrap();

        save_collection(&base_path, "test", &collection)
            .await
            .unwrap();

        // Load collection
        let loaded = load_collection(&base_path, "test").await.unwrap();

        assert_eq!(loaded.name(), "test");
        assert_eq!(loaded.dimensions(), 3);
        assert_eq!(loaded.metric(), DistanceMetric::Cosine);
    }

    /// Test that vectors are actually persisted and can be retrieved after reload.
    /// This is a regression test for the bug where vectors.json was saved empty.
    #[tokio::test]
    async fn test_vector_persistence_regression() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create collection with multiple vectors
        let collection = Collection::new(
            "persist_test".to_string(),
            3,
            DistanceMetric::Cosine,
            HnswConfig::default(),
        )
        .unwrap();

        // Insert multiple vectors with metadata
        let mut meta1 = VectorMetadata::new();
        meta1.insert("doc_id", "doc1");
        meta1.insert("category", "rust");

        let mut meta2 = VectorMetadata::new();
        meta2.insert("doc_id", "doc2");
        meta2.insert("category", "python");

        collection
            .insert("vec1", &[1.0, 0.0, 0.0], Some(meta1.clone()))
            .unwrap();
        collection
            .insert("vec2", &[0.0, 1.0, 0.0], Some(meta2.clone()))
            .unwrap();
        collection.insert("vec3", &[0.0, 0.0, 1.0], None).unwrap();

        // Verify vectors are in collection before save
        assert_eq!(collection.len(), 3);

        // Save collection
        save_collection(&base_path, "persist_test", &collection)
            .await
            .unwrap();

        // Verify vectors.json file exists and is not empty
        let vectors_path = base_path.join("persist_test").join("vectors.json");
        assert!(vectors_path.exists(), "vectors.json should exist");
        let vectors_json = tokio::fs::read_to_string(&vectors_path).await.unwrap();
        let stored_vectors: Vec<StoredVectorData> =
            serde_json::from_str(&vectors_json).expect("vectors.json should be valid JSON");
        assert_eq!(
            stored_vectors.len(),
            3,
            "vectors.json should contain 3 vectors"
        );

        // Load collection (simulating server restart)
        let loaded = load_collection(&base_path, "persist_test").await.unwrap();

        // Verify all vectors are present
        assert_eq!(loaded.len(), 3, "Loaded collection should have 3 vectors");

        // Verify vectors can be retrieved by ID (get returns (Vec<f32>, Option<VectorMetadata>))
        let (v1_vec, _v1_meta) = loaded.get("vec1").expect("vec1 should exist");
        assert_eq!(v1_vec.len(), 3);
        assert!((v1_vec[0] - 1.0).abs() < 0.0001);

        let (v2_vec, _v2_meta) = loaded.get("vec2").expect("vec2 should exist");
        assert_eq!(v2_vec.len(), 3);
        assert!((v2_vec[1] - 1.0).abs() < 0.0001);

        let (v3_vec, _v3_meta) = loaded.get("vec3").expect("vec3 should exist");
        assert_eq!(v3_vec.len(), 3);
        assert!((v3_vec[2] - 1.0).abs() < 0.0001);

        // Verify search works on loaded collection
        let results = loaded.search(&[1.0, 0.0, 0.0], 3).unwrap();
        assert!(!results.is_empty(), "Search should return results");
        assert_eq!(results[0].id, "vec1", "vec1 should be the closest match");
    }

    /// Test that metadata is correctly persisted and loaded.
    #[tokio::test]
    async fn test_metadata_persistence() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        let collection = Collection::new(
            "meta_test".to_string(),
            3,
            DistanceMetric::Euclidean,
            HnswConfig::default(),
        )
        .unwrap();

        let mut meta = VectorMetadata::new();
        meta.insert("title", "Test Document");
        meta.insert("page", 42i64);
        meta.insert("score", 0.95f64);
        meta.insert("published", true);

        collection
            .insert("doc1", &[1.0, 2.0, 3.0], Some(meta))
            .unwrap();

        save_collection(&base_path, "meta_test", &collection)
            .await
            .unwrap();

        let loaded = load_collection(&base_path, "meta_test").await.unwrap();

        // Export and check metadata
        let exported = loaded.export_all();
        assert_eq!(exported.len(), 1);

        let (id, _vector, loaded_meta) = &exported[0];
        assert_eq!(id, "doc1");
        let loaded_meta = loaded_meta.as_ref().expect("metadata should exist");

        // Use VectorMetadata helper methods
        assert_eq!(loaded_meta.get_string("title"), Some("Test Document"));
        assert_eq!(loaded_meta.get_int("page"), Some(42));
        assert!((loaded_meta.get_float("score").unwrap() - 0.95).abs() < 0.0001);
        assert_eq!(loaded_meta.get_bool("published"), Some(true));
    }
}
