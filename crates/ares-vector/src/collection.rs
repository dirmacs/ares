//! Vector collection.
//!
//! A collection is a named container for vectors with a specific dimensionality
//! and distance metric.

use crate::config::HnswConfig;
use crate::distance::DistanceMetric;
use crate::error::Result;
use crate::index::HnswIndex;
use crate::types::{SearchResult, VectorMetadata};
use crate::{CollectionStats, HnswParams};
use std::sync::Arc;

/// A named collection of vectors.
///
/// Each collection has:
/// - A unique name
/// - Fixed vector dimensions
/// - A distance metric for similarity calculations
/// - An HNSW index for fast approximate nearest neighbor search
pub struct Collection {
    /// Collection name.
    name: String,
    /// Vector dimensions.
    dimensions: usize,
    /// Distance metric.
    metric: DistanceMetric,
    /// The underlying HNSW index.
    index: Arc<HnswIndex>,
    /// HNSW configuration.
    hnsw_config: HnswConfig,
}

impl Collection {
    /// Create a new collection.
    pub fn new(
        name: String,
        dimensions: usize,
        metric: DistanceMetric,
        hnsw_config: HnswConfig,
    ) -> Result<Self> {
        let index = HnswIndex::new(dimensions, metric, hnsw_config.clone())?;

        Ok(Self {
            name,
            dimensions,
            metric,
            index: Arc::new(index),
            hnsw_config,
        })
    }

    /// Get the collection name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the vector dimensions.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Get the distance metric.
    pub fn metric(&self) -> DistanceMetric {
        self.metric
    }

    /// Get the number of vectors in the collection.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Check if the collection is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Insert a vector.
    pub fn insert(&self, id: &str, vector: &[f32], metadata: Option<VectorMetadata>) -> Result<()> {
        self.index.insert(id, vector, metadata)
    }

    /// Insert multiple vectors in batch.
    pub fn insert_batch<'a, I>(&self, vectors: I) -> Result<usize>
    where
        I: IntoIterator<Item = (&'a str, &'a [f32], Option<VectorMetadata>)>,
    {
        self.index.insert_batch(vectors)
    }

    /// Update a vector.
    pub fn update(&self, id: &str, vector: &[f32], metadata: Option<VectorMetadata>) -> Result<()> {
        self.index.update(id, vector, metadata)
    }

    /// Delete a vector.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.index.delete(id)
    }

    /// Delete multiple vectors.
    pub fn delete_batch(&self, ids: &[&str]) -> Result<usize> {
        self.index.delete_batch(ids)
    }

    /// Search for similar vectors.
    pub fn search(&self, query: &[f32], limit: usize) -> Result<Vec<SearchResult>> {
        self.index.search(query, limit)
    }

    /// Search with a minimum score threshold.
    pub fn search_with_threshold(
        &self,
        query: &[f32],
        limit: usize,
        min_score: f32,
    ) -> Result<Vec<SearchResult>> {
        self.index.search_with_threshold(query, limit, min_score)
    }

    /// Get a vector by ID.
    pub fn get(&self, id: &str) -> Option<(Vec<f32>, Option<VectorMetadata>)> {
        self.index.get(id)
    }

    /// Check if a vector exists.
    pub fn contains(&self, id: &str) -> bool {
        self.index.contains(id)
    }

    /// Compact the index.
    pub fn compact(&self) -> Result<()> {
        self.index.compact()
    }

    /// Get collection statistics.
    pub fn stats(&self) -> CollectionStats {
        CollectionStats {
            name: self.name.clone(),
            vector_count: self.index.len(),
            dimensions: self.dimensions,
            metric: self.metric,
            memory_bytes: self.index.memory_usage(),
            hnsw_params: HnswParams {
                m: self.hnsw_config.m,
                ef_construction: self.hnsw_config.ef_construction,
                ef_search: self.hnsw_config.ef_search,
            },
        }
    }

    /// Get the HNSW configuration.
    pub fn hnsw_config(&self) -> &HnswConfig {
        &self.hnsw_config
    }

    /// Get a reference to the underlying index.
    #[allow(dead_code)]
    pub(crate) fn index(&self) -> &Arc<HnswIndex> {
        &self.index
    }

    /// Export all vectors for persistence.
    ///
    /// Returns a vector of (id, vector, metadata) tuples.
    pub fn export_all(&self) -> Vec<(String, Vec<f32>, Option<VectorMetadata>)> {
        self.index.export_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> HnswConfig {
        HnswConfig::default()
    }

    #[test]
    fn test_collection_basic() {
        let col = Collection::new(
            "test".to_string(),
            3,
            DistanceMetric::Cosine,
            default_config(),
        )
        .unwrap();

        assert_eq!(col.name(), "test");
        assert_eq!(col.dimensions(), 3);
        assert_eq!(col.metric(), DistanceMetric::Cosine);
        assert!(col.is_empty());
    }

    #[test]
    fn test_collection_operations() {
        let col = Collection::new(
            "test".to_string(),
            3,
            DistanceMetric::Cosine,
            default_config(),
        )
        .unwrap();

        col.insert("vec1", &[1.0, 0.0, 0.0], None).unwrap();
        col.insert("vec2", &[0.0, 1.0, 0.0], None).unwrap();

        assert_eq!(col.len(), 2);
        assert!(col.contains("vec1"));
        assert!(col.contains("vec2"));
        assert!(!col.contains("vec3"));

        let results = col.search(&[1.0, 0.0, 0.0], 10).unwrap();
        assert!(!results.is_empty());

        col.delete("vec1").unwrap();
        assert!(!col.contains("vec1"));
        assert_eq!(col.len(), 1);
    }

    #[test]
    fn test_collection_stats() {
        let col = Collection::new(
            "test".to_string(),
            128,
            DistanceMetric::Euclidean,
            HnswConfig::accurate(),
        )
        .unwrap();

        col.insert("vec1", &vec![0.0; 128], None).unwrap();

        let stats = col.stats();
        assert_eq!(stats.name, "test");
        assert_eq!(stats.vector_count, 1);
        assert_eq!(stats.dimensions, 128);
        assert_eq!(stats.metric, DistanceMetric::Euclidean);
        assert!(stats.memory_bytes > 0);
    }
}
