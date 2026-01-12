//! HNSW index wrapper.
//!
//! This module wraps the hnsw_rs library to provide a simpler interface
//! and additional functionality like ID mapping.

use crate::config::HnswConfig;
use crate::distance::DistanceMetric;
use crate::error::{Error, Result};
use crate::types::{SearchResult, VectorId, VectorMetadata};
use anndists::dist::distances::{DistCosine, DistDot, DistL1, DistL2};
use hnsw_rs::hnsw::Hnsw;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{debug, trace};

/// Thread-safe HNSW index with ID mapping.
pub struct HnswIndex {
    /// The underlying HNSW index (boxed for type erasure).
    inner: RwLock<IndexInner>,
    /// Mapping from string IDs to internal numeric IDs.
    id_to_internal: RwLock<HashMap<VectorId, usize>>,
    /// Mapping from internal numeric IDs to string IDs.
    internal_to_id: RwLock<HashMap<usize, VectorId>>,
    /// Stored vectors for retrieval.
    vectors: RwLock<HashMap<usize, Vec<f32>>>,
    /// Stored metadata.
    metadata: RwLock<HashMap<usize, VectorMetadata>>,
    /// Counter for generating internal IDs.
    next_internal_id: AtomicUsize,
    /// Vector dimensions.
    dimensions: usize,
    /// Distance metric.
    metric: DistanceMetric,
    /// HNSW configuration.
    config: HnswConfig,
}

/// Type-erased inner index.
enum IndexInner {
    Cosine(Hnsw<'static, f32, DistCosine>),
    Euclidean(Hnsw<'static, f32, DistL2>),
    DotProduct(Hnsw<'static, f32, DistDot>),
    Manhattan(Hnsw<'static, f32, DistL1>),
}

impl HnswIndex {
    /// Create a new HNSW index.
    ///
    /// # Arguments
    ///
    /// * `dimensions` - Dimensionality of vectors.
    /// * `metric` - Distance metric to use.
    /// * `config` - HNSW configuration.
    pub fn new(dimensions: usize, metric: DistanceMetric, config: HnswConfig) -> Result<Self> {
        if dimensions == 0 {
            return Err(Error::InvalidVector("Dimensions must be > 0".to_string()));
        }

        let max_elements = 1_000_000; // Initial capacity
        let max_layer = 16;

        let inner = match metric {
            DistanceMetric::Cosine => {
                let hnsw = Hnsw::new(
                    config.m,
                    max_elements,
                    max_layer,
                    config.ef_construction,
                    DistCosine {},
                );
                IndexInner::Cosine(hnsw)
            }
            DistanceMetric::Euclidean => {
                let hnsw = Hnsw::new(
                    config.m,
                    max_elements,
                    max_layer,
                    config.ef_construction,
                    DistL2 {},
                );
                IndexInner::Euclidean(hnsw)
            }
            DistanceMetric::DotProduct => {
                let hnsw = Hnsw::new(
                    config.m,
                    max_elements,
                    max_layer,
                    config.ef_construction,
                    DistDot {},
                );
                IndexInner::DotProduct(hnsw)
            }
            DistanceMetric::Manhattan => {
                let hnsw = Hnsw::new(
                    config.m,
                    max_elements,
                    max_layer,
                    config.ef_construction,
                    DistL1 {},
                );
                IndexInner::Manhattan(hnsw)
            }
        };

        Ok(Self {
            inner: RwLock::new(inner),
            id_to_internal: RwLock::new(HashMap::new()),
            internal_to_id: RwLock::new(HashMap::new()),
            vectors: RwLock::new(HashMap::new()),
            metadata: RwLock::new(HashMap::new()),
            next_internal_id: AtomicUsize::new(0),
            dimensions,
            metric,
            config,
        })
    }

    /// Get the vector dimensions.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Get the distance metric.
    pub fn metric(&self) -> DistanceMetric {
        self.metric
    }

    /// Get the number of vectors in the index.
    pub fn len(&self) -> usize {
        self.id_to_internal.read().len()
    }

    /// Check if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Check if a vector exists.
    pub fn contains(&self, id: &str) -> bool {
        self.id_to_internal.read().contains_key(id)
    }

    /// Insert a vector into the index.
    ///
    /// If a vector with the same ID exists, it will be updated.
    pub fn insert(&self, id: &str, vector: &[f32], meta: Option<VectorMetadata>) -> Result<()> {
        // Validate dimensions
        if vector.len() != self.dimensions {
            return Err(Error::DimensionMismatch {
                expected: self.dimensions,
                actual: vector.len(),
            });
        }

        // Validate vector values
        if vector.iter().any(|v| v.is_nan() || v.is_infinite()) {
            return Err(Error::InvalidVector(
                "Vector contains NaN or Inf".to_string(),
            ));
        }

        // Check if this is an update
        let internal_id = {
            let id_map = self.id_to_internal.read();
            if let Some(&existing_id) = id_map.get(id) {
                // Update: reuse internal ID
                existing_id
            } else {
                // Insert: generate new internal ID
                self.next_internal_id.fetch_add(1, Ordering::SeqCst)
            }
        };

        // Store mappings
        {
            let mut id_to_internal = self.id_to_internal.write();
            let mut internal_to_id = self.internal_to_id.write();
            id_to_internal.insert(id.to_string(), internal_id);
            internal_to_id.insert(internal_id, id.to_string());
        }

        // Store vector
        {
            let mut vectors = self.vectors.write();
            vectors.insert(internal_id, vector.to_vec());
        }

        // Store metadata
        if let Some(m) = meta {
            let mut metadata = self.metadata.write();
            metadata.insert(internal_id, m);
        }

        // Insert into HNSW index
        let inner = self.inner.write();
        match &*inner {
            IndexInner::Cosine(hnsw) => {
                hnsw.insert((vector, internal_id));
            }
            IndexInner::Euclidean(hnsw) => {
                hnsw.insert((vector, internal_id));
            }
            IndexInner::DotProduct(hnsw) => {
                hnsw.insert((vector, internal_id));
            }
            IndexInner::Manhattan(hnsw) => {
                hnsw.insert((vector, internal_id));
            }
        }

        trace!(id, internal_id, "Inserted vector");
        Ok(())
    }

    /// Insert multiple vectors in batch.
    ///
    /// More efficient than calling `insert` repeatedly.
    pub fn insert_batch<'a, I>(&self, vectors: I) -> Result<usize>
    where
        I: IntoIterator<Item = (&'a str, &'a [f32], Option<VectorMetadata>)>,
    {
        let mut count = 0;
        let mut batch_data: Vec<(Vec<f32>, usize)> = Vec::new();

        for (id, vector, meta) in vectors {
            // Validate
            if vector.len() != self.dimensions {
                return Err(Error::DimensionMismatch {
                    expected: self.dimensions,
                    actual: vector.len(),
                });
            }

            if vector.iter().any(|v| v.is_nan() || v.is_infinite()) {
                return Err(Error::InvalidVector(format!(
                    "Vector '{}' contains NaN or Inf",
                    id
                )));
            }

            let internal_id = {
                let id_map = self.id_to_internal.read();
                id_map
                    .get(id)
                    .copied()
                    .unwrap_or_else(|| self.next_internal_id.fetch_add(1, Ordering::SeqCst))
            };

            // Store mappings
            {
                let mut id_to_internal = self.id_to_internal.write();
                let mut internal_to_id = self.internal_to_id.write();
                id_to_internal.insert(id.to_string(), internal_id);
                internal_to_id.insert(internal_id, id.to_string());
            }

            // Store vector and metadata
            {
                let mut vectors = self.vectors.write();
                vectors.insert(internal_id, vector.to_vec());
            }

            if let Some(m) = meta {
                let mut metadata = self.metadata.write();
                metadata.insert(internal_id, m);
            }

            batch_data.push((vector.to_vec(), internal_id));
            count += 1;
        }

        // Batch insert into HNSW
        if !batch_data.is_empty() {
            let inner = self.inner.write();
            let refs: Vec<(&Vec<f32>, usize)> = batch_data.iter().map(|(v, id)| (v, *id)).collect();

            match &*inner {
                IndexInner::Cosine(hnsw) => {
                    if self.config.parallel_construction {
                        hnsw.parallel_insert(&refs);
                    } else {
                        for (v, id) in refs {
                            hnsw.insert((v, id));
                        }
                    }
                }
                IndexInner::Euclidean(hnsw) => {
                    if self.config.parallel_construction {
                        hnsw.parallel_insert(&refs);
                    } else {
                        for (v, id) in refs {
                            hnsw.insert((v, id));
                        }
                    }
                }
                IndexInner::DotProduct(hnsw) => {
                    if self.config.parallel_construction {
                        hnsw.parallel_insert(&refs);
                    } else {
                        for (v, id) in refs {
                            hnsw.insert((v, id));
                        }
                    }
                }
                IndexInner::Manhattan(hnsw) => {
                    if self.config.parallel_construction {
                        hnsw.parallel_insert(&refs);
                    } else {
                        for (v, id) in refs {
                            hnsw.insert((v, id));
                        }
                    }
                }
            }
        }

        debug!(count, "Batch inserted vectors");
        Ok(count)
    }

    /// Delete a vector from the index.
    ///
    /// Note: HNSW doesn't support true deletion. The vector is marked as
    /// deleted but still occupies space until compaction.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let internal_id = {
            let mut id_to_internal = self.id_to_internal.write();
            let Some(internal_id) = id_to_internal.remove(id) else {
                return Ok(false);
            };
            internal_id
        };

        // Remove from mappings
        {
            let mut internal_to_id = self.internal_to_id.write();
            internal_to_id.remove(&internal_id);
        }

        // Remove stored data
        {
            let mut vectors = self.vectors.write();
            vectors.remove(&internal_id);
        }

        {
            let mut metadata = self.metadata.write();
            metadata.remove(&internal_id);
        }

        // Note: HNSW doesn't have a delete method, so the point remains
        // in the index but won't be returned in results since we removed
        // the ID mapping. A compaction/rebuild would remove it fully.

        trace!(id, internal_id, "Deleted vector");
        Ok(true)
    }

    /// Delete multiple vectors.
    pub fn delete_batch(&self, ids: &[&str]) -> Result<usize> {
        let mut count = 0;
        for id in ids {
            if self.delete(id)? {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Search for similar vectors.
    pub fn search(&self, query: &[f32], limit: usize) -> Result<Vec<SearchResult>> {
        if query.len() != self.dimensions {
            return Err(Error::DimensionMismatch {
                expected: self.dimensions,
                actual: query.len(),
            });
        }

        let ef_search = std::cmp::max(self.config.ef_search, limit);
        let inner = self.inner.read();

        let neighbors = match &*inner {
            IndexInner::Cosine(hnsw) => hnsw.search(query, limit, ef_search),
            IndexInner::Euclidean(hnsw) => hnsw.search(query, limit, ef_search),
            IndexInner::DotProduct(hnsw) => hnsw.search(query, limit, ef_search),
            IndexInner::Manhattan(hnsw) => hnsw.search(query, limit, ef_search),
        };

        let internal_to_id = self.internal_to_id.read();
        let metadata = self.metadata.read();

        let results: Vec<SearchResult> = neighbors
            .into_iter()
            .filter_map(|neighbor| {
                let internal_id = neighbor.d_id;
                let id = internal_to_id.get(&internal_id)?;

                // Convert distance to similarity score
                let score = self.distance_to_score(neighbor.distance);

                Some(SearchResult {
                    id: id.clone(),
                    score,
                    metadata: metadata.get(&internal_id).cloned(),
                })
            })
            .collect();

        Ok(results)
    }

    /// Search with a minimum score threshold.
    pub fn search_with_threshold(
        &self,
        query: &[f32],
        limit: usize,
        min_score: f32,
    ) -> Result<Vec<SearchResult>> {
        let results = self.search(query, limit)?;
        Ok(results
            .into_iter()
            .filter(|r| r.score >= min_score)
            .collect())
    }

    /// Get a vector by ID.
    pub fn get(&self, id: &str) -> Option<(Vec<f32>, Option<VectorMetadata>)> {
        let internal_id = *self.id_to_internal.read().get(id)?;
        let vector = self.vectors.read().get(&internal_id)?.clone();
        let meta = self.metadata.read().get(&internal_id).cloned();
        Some((vector, meta))
    }

    /// Update a vector.
    pub fn update(&self, id: &str, vector: &[f32], meta: Option<VectorMetadata>) -> Result<()> {
        if !self.contains(id) {
            return Err(Error::VectorNotFound(id.to_string()));
        }
        self.insert(id, vector, meta)
    }

    /// Compact the index by rebuilding it.
    ///
    /// This removes deleted vectors and optimizes the graph structure.
    pub fn compact(&self) -> Result<()> {
        // Collect all valid vectors
        let id_to_internal = self.id_to_internal.read();
        let vectors = self.vectors.read();
        let metadata = self.metadata.read();

        let valid_data: Vec<_> = id_to_internal
            .iter()
            .filter_map(|(id, &internal_id)| {
                let vector = vectors.get(&internal_id)?;
                let meta = metadata.get(&internal_id).cloned();
                Some((id.clone(), vector.clone(), meta))
            })
            .collect();

        drop(id_to_internal);
        drop(vectors);
        drop(metadata);

        // Clear existing data
        self.id_to_internal.write().clear();
        self.internal_to_id.write().clear();
        self.vectors.write().clear();
        self.metadata.write().clear();
        self.next_internal_id.store(0, Ordering::SeqCst);

        // Rebuild index
        let max_elements = valid_data.len().max(1_000_000);
        let max_layer = 16;

        let new_inner = match self.metric {
            DistanceMetric::Cosine => IndexInner::Cosine(Hnsw::new(
                self.config.m,
                max_elements,
                max_layer,
                self.config.ef_construction,
                DistCosine {},
            )),
            DistanceMetric::Euclidean => IndexInner::Euclidean(Hnsw::new(
                self.config.m,
                max_elements,
                max_layer,
                self.config.ef_construction,
                DistL2 {},
            )),
            DistanceMetric::DotProduct => IndexInner::DotProduct(Hnsw::new(
                self.config.m,
                max_elements,
                max_layer,
                self.config.ef_construction,
                DistDot {},
            )),
            DistanceMetric::Manhattan => IndexInner::Manhattan(Hnsw::new(
                self.config.m,
                max_elements,
                max_layer,
                self.config.ef_construction,
                DistL1 {},
            )),
        };

        *self.inner.write() = new_inner;

        // Re-insert all vectors
        let batch: Vec<_> = valid_data
            .iter()
            .map(|(id, v, m)| (id.as_str(), v.as_slice(), m.clone()))
            .collect();

        self.insert_batch(batch)?;

        debug!(count = valid_data.len(), "Compacted index");
        Ok(())
    }

    /// Estimate memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        let vectors = self.vectors.read();
        let metadata = self.metadata.read();

        // Vector storage
        let vector_bytes: usize = vectors.values().map(|v| v.len() * 4).sum();

        // ID mappings (rough estimate)
        let id_bytes: usize = self.id_to_internal.read().keys().map(|s| s.len()).sum();

        // Metadata (rough estimate)
        let meta_bytes: usize = metadata.len() * 100; // Rough estimate

        // HNSW graph (rough estimate: ~M * 4 bytes per connection per vector)
        let graph_bytes = vectors.len() * self.config.m * 4 * 16; // Approximate

        vector_bytes + id_bytes + meta_bytes + graph_bytes
    }

    /// Convert HNSW distance to a similarity score (higher = more similar).
    fn distance_to_score(&self, distance: f32) -> f32 {
        match self.metric {
            DistanceMetric::Cosine => {
                // HNSW uses 1 - cos_sim as distance, so score = 1 - distance
                1.0 - distance
            }
            DistanceMetric::DotProduct => {
                // Higher dot product = more similar, HNSW may negate it
                -distance
            }
            DistanceMetric::Euclidean | DistanceMetric::Manhattan => {
                // Transform distance to similarity: 1 / (1 + dist)
                1.0 / (1.0 + distance)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MetadataValue;

    fn default_config() -> HnswConfig {
        HnswConfig::default()
    }

    #[test]
    fn test_insert_and_search() {
        let index = HnswIndex::new(3, DistanceMetric::Cosine, default_config()).unwrap();

        index.insert("vec1", &[1.0, 0.0, 0.0], None).unwrap();
        index.insert("vec2", &[0.0, 1.0, 0.0], None).unwrap();
        index.insert("vec3", &[0.9, 0.1, 0.0], None).unwrap();

        assert_eq!(index.len(), 3);

        let results = index.search(&[1.0, 0.0, 0.0], 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "vec1");
    }

    #[test]
    fn test_dimension_mismatch() {
        let index = HnswIndex::new(3, DistanceMetric::Cosine, default_config()).unwrap();

        let result = index.insert("vec1", &[1.0, 0.0], None);
        assert!(matches!(result, Err(Error::DimensionMismatch { .. })));
    }

    #[test]
    fn test_delete() {
        let index = HnswIndex::new(3, DistanceMetric::Cosine, default_config()).unwrap();

        index.insert("vec1", &[1.0, 0.0, 0.0], None).unwrap();
        assert_eq!(index.len(), 1);

        let deleted = index.delete("vec1").unwrap();
        assert!(deleted);
        assert_eq!(index.len(), 0);

        let deleted_again = index.delete("vec1").unwrap();
        assert!(!deleted_again);
    }

    #[test]
    fn test_get() {
        let index = HnswIndex::new(3, DistanceMetric::Cosine, default_config()).unwrap();

        let meta =
            VectorMetadata::from_pairs([("key", MetadataValue::String("value".to_string()))]);
        index.insert("vec1", &[1.0, 2.0, 3.0], Some(meta)).unwrap();

        let (vector, metadata) = index.get("vec1").unwrap();
        assert_eq!(vector, vec![1.0, 2.0, 3.0]);
        assert!(metadata.is_some());
    }

    #[test]
    fn test_contains() {
        let index = HnswIndex::new(3, DistanceMetric::Cosine, default_config()).unwrap();

        assert!(!index.contains("vec1"));
        index.insert("vec1", &[1.0, 0.0, 0.0], None).unwrap();
        assert!(index.contains("vec1"));
    }
}
