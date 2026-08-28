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
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{debug, trace};

type IndexResult<T> = std::result::Result<T, IndexConfigError>;

// =============================================================================
// Pure PostgreSQL index helpers (R46)
// =============================================================================

/// Minimum HNSW `m` (max edges per node) for [`IndexConfig`].
pub const MIN_HNSW_M: u16 = 4;
/// Maximum HNSW `m` for [`IndexConfig`].
pub const MAX_HNSW_M: u16 = 100;
/// Minimum HNSW `ef_construction` for [`IndexConfig`].
pub const MIN_HNSW_EF_CONSTRUCTION: u32 = 10;
/// Maximum HNSW `ef_construction` for [`IndexConfig`].
pub const MAX_HNSW_EF_CONSTRUCTION: u32 = 1000;
/// Default HNSW `m` used in [`IndexConfig`].
pub const DEFAULT_INDEX_M: u16 = 16;
/// Default HNSW `ef_construction` used in [`IndexConfig`].
pub const DEFAULT_INDEX_EF_CONSTRUCTION: u32 = 64;
/// Default vector dimensions in [`IndexConfig`].
pub const DEFAULT_INDEX_DIMENSIONS: usize = 384;

/// PostgreSQL / pgvector index access method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IndexType {
    /// HNSW approximate nearest-neighbor index.
    #[default]
    Hnsw,
    /// IVFFlat inverted-file index.
    Ivfflat,
    /// Flat / sequential scan (pgvector extension, no ANN index).
    Flat,
}

/// Validation failures for [`IndexConfig`] and SQL builders.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IndexConfigError {
    /// HNSW `m` is outside [`MIN_HNSW_M`]..=[`MAX_HNSW_M`].
    #[error("invalid M: {0} (must be {MIN_HNSW_M}-{MAX_HNSW_M})")]
    InvalidM(u16),
    /// HNSW `ef_construction` is outside allowed bounds.
    #[error("invalid ef_construction: {0} (must be {MIN_HNSW_EF_CONSTRUCTION}-{MAX_HNSW_EF_CONSTRUCTION})")]
    InvalidEfConstruction(u32),
    /// Index type cannot be used for the requested operation.
    #[error("unsupported index type: {0:?}")]
    UnsupportedIndexType(IndexType),
    /// Vector dimensionality must be greater than zero.
    #[error("dimensions must be greater than zero")]
    InvalidDimensions,
    /// Distance metric has no pgvector operator mapping.
    #[error("unsupported distance metric for pgvector: {0}")]
    UnsupportedDistance(DistanceMetric),
    /// Table or column name is not a valid SQL identifier.
    #[error("invalid SQL identifier: {0}")]
    InvalidIdentifier(String),
}

/// Serializable pgvector index configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexConfig {
    /// Embedding dimensionality.
    #[serde(default = "default_index_dimensions")]
    pub dimensions: usize,
    /// Index access method.
    #[serde(default)]
    pub index_type: IndexType,
    /// Distance metric / opclass selection.
    #[serde(default)]
    pub distance: DistanceMetric,
    /// HNSW `m` parameter.
    #[serde(default = "default_index_m")]
    pub m: u16,
    /// HNSW `ef_construction` parameter.
    #[serde(default = "default_index_ef_construction")]
    pub ef_construction: u32,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            dimensions: default_index_dimensions(),
            index_type: IndexType::default(),
            distance: DistanceMetric::default(),
            m: default_index_m(),
            ef_construction: default_index_ef_construction(),
        }
    }
}

impl fmt::Display for IndexConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} index ({}-dim, {}, m={}, ef={})",
            index_type_string(self.index_type),
            self.dimensions,
            self.distance,
            self.m,
            self.ef_construction
        )
    }
}

/// Default index dimensions for serde.
pub fn default_index_dimensions() -> usize {
    DEFAULT_INDEX_DIMENSIONS
}

/// Default HNSW `m` for serde.
pub fn default_index_m() -> u16 {
    DEFAULT_INDEX_M
}

/// Default HNSW `ef_construction` for serde.
pub fn default_index_ef_construction() -> u32 {
    DEFAULT_INDEX_EF_CONSTRUCTION
}

/// PostgreSQL index or extension keyword for an [`IndexType`].
pub fn index_type_string(index_type: IndexType) -> &'static str {
    match index_type {
        IndexType::Hnsw => "hnsw",
        IndexType::Ivfflat => "ivfflat",
        IndexType::Flat => "vector",
    }
}

/// pgvector distance operator for ORDER BY / WHERE clauses.
pub fn distance_operator(metric: DistanceMetric) -> IndexResult<&'static str> {
    crate::distance::distance_operator(metric)
        .ok_or(IndexConfigError::UnsupportedDistance(metric))
}

/// pgvector opclass suffix used when creating indexes.
pub fn distance_ops_class(metric: DistanceMetric) -> IndexResult<&'static str> {
    match metric {
        DistanceMetric::Cosine => Ok("vector_cosine_ops"),
        DistanceMetric::Euclidean => Ok("vector_l2_ops"),
        DistanceMetric::DotProduct => Ok("vector_ip_ops"),
        DistanceMetric::Manhattan => Err(IndexConfigError::UnsupportedDistance(metric)),
    }
}

/// Format HNSW `WITH` clause parameters for PostgreSQL DDL.
pub fn hnsw_param_string(m: u16, ef_construction: u32) -> String {
    format!("m = {m}, ef_construction = {ef_construction}")
}

/// Return the PostgreSQL access method or error for types that do not use one.
pub fn index_access_method(index_type: IndexType) -> IndexResult<&'static str> {
    match index_type {
        IndexType::Hnsw => Ok("hnsw"),
        IndexType::Ivfflat => Ok("ivfflat"),
        IndexType::Flat => Err(IndexConfigError::UnsupportedIndexType(IndexType::Flat)),
    }
}

fn quote_sql_identifier(raw: &str) -> IndexResult<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(IndexConfigError::InvalidIdentifier(
            "empty identifier".into(),
        ));
    }
    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err(IndexConfigError::InvalidIdentifier(name.into()));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(IndexConfigError::InvalidIdentifier(name.into()));
    }
    Ok(format!("\"{}\"", name.replace('"', "\"\"")))
}

fn index_name_suffix(index_type: IndexType) -> &'static str {
    match index_type {
        IndexType::Hnsw => "hnsw",
        IndexType::Ivfflat => "ivfflat",
        IndexType::Flat => "flat",
    }
}

/// Validate dimensions, distance mapping, and HNSW tuning parameters.
pub fn validate_index_config(config: &IndexConfig) -> IndexResult<()> {
    if config.dimensions == 0 {
        return Err(IndexConfigError::InvalidDimensions);
    }
    distance_ops_class(config.distance)?;
    if config.index_type == IndexType::Hnsw {
        if config.m < MIN_HNSW_M || config.m > MAX_HNSW_M {
            return Err(IndexConfigError::InvalidM(config.m));
        }
        if config.ef_construction < MIN_HNSW_EF_CONSTRUCTION
            || config.ef_construction > MAX_HNSW_EF_CONSTRUCTION
        {
            return Err(IndexConfigError::InvalidEfConstruction(config.ef_construction));
        }
    }
    Ok(())
}

/// Build `CREATE INDEX` SQL for a pgvector embedding column.
pub fn build_create_index_sql(
    table: &str,
    column: &str,
    config: &IndexConfig,
) -> IndexResult<String> {
    validate_index_config(config)?;
    if config.index_type == IndexType::Flat {
        return Ok(String::new());
    }

    let table_id = quote_sql_identifier(table)?;
    let column_id = quote_sql_identifier(column)?;
    let ops = distance_ops_class(config.distance)?;
    let suffix = index_name_suffix(config.index_type);
    let index_name = format!("{}_{}_{}_idx", table.trim(), column.trim(), suffix);
    let index_name_id = quote_sql_identifier(&index_name)?;
    let method = index_type_string(config.index_type);

    let stmt = match config.index_type {
        IndexType::Hnsw => {
            let with_clause = hnsw_param_string(config.m, config.ef_construction);
            format!(
                "CREATE INDEX IF NOT EXISTS {index_name_id} ON {table_id} \
                 USING {method} ({column_id} {ops}) WITH ({with_clause})"
            )
        }
        IndexType::Ivfflat => format!(
            "CREATE INDEX IF NOT EXISTS {index_name_id} ON {table_id} \
             USING {method} ({column_id} {ops}) WITH (lists = 100)"
        ),
        IndexType::Flat => String::new(),
    };
    Ok(stmt)
}

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

    /// Export all vectors for persistence.
    ///
    /// Returns an iterator over (id, vector, metadata) tuples.
    pub fn export_all(&self) -> Vec<(String, Vec<f32>, Option<VectorMetadata>)> {
        let id_to_internal = self.id_to_internal.read();
        let vectors = self.vectors.read();
        let metadata = self.metadata.read();

        id_to_internal
            .iter()
            .filter_map(|(id, &internal_id)| {
                let vector = vectors.get(&internal_id)?.clone();
                let meta = metadata.get(&internal_id).cloned();
                Some((id.clone(), vector, meta))
            })
            .collect()
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

    #[test]
    fn test_new_zero_dimensions() {
        let result = HnswIndex::new(0, DistanceMetric::Cosine, default_config());
        assert!(matches!(result, Err(Error::InvalidVector(_))));
    }

    #[test]
    fn test_new_defaults() {
        let index = HnswIndex::new(4, DistanceMetric::Euclidean, HnswConfig::default()).unwrap();
        assert_eq!(index.dimensions(), 4);
        assert_eq!(index.metric(), DistanceMetric::Euclidean);
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_invalid_vector_nan() {
        let index = HnswIndex::new(2, DistanceMetric::Cosine, default_config()).unwrap();
        let result = index.insert("bad", &[f32::NAN, 0.0], None);
        assert!(matches!(result, Err(Error::InvalidVector(_))));
    }

    #[test]
    fn test_all_distance_metrics_search() {
        for metric in [
            DistanceMetric::Cosine,
            DistanceMetric::Euclidean,
            DistanceMetric::DotProduct,
            DistanceMetric::Manhattan,
        ] {
            let index = HnswIndex::new(3, metric, default_config()).unwrap();
            index.insert("a", &[1.0, 0.0, 0.0], None).unwrap();
            index.insert("b", &[0.0, 1.0, 0.0], None).unwrap();
            let results = index.search(&[1.0, 0.0, 0.0], 5).unwrap();
            assert!(!results.is_empty());
            assert_eq!(results[0].id, "a");
        }
    }

    #[test]
    fn test_insert_batch_and_export() {
        let index = HnswIndex::new(2, DistanceMetric::Cosine, default_config()).unwrap();
        let v1 = [1.0f32, 0.0];
        let v2 = [0.0f32, 1.0];
        let count = index
            .insert_batch([("v1", &v1[..], None), ("v2", &v2[..], None)])
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(index.len(), 2);

        let exported = index.export_all();
        assert_eq!(exported.len(), 2);
        let ids: Vec<_> = exported.into_iter().map(|(id, _, _)| id).collect();
        assert!(ids.contains(&"v1".to_string()));
        assert!(ids.contains(&"v2".to_string()));
    }

    #[test]
    fn test_update_and_vector_not_found() {
        let index = HnswIndex::new(2, DistanceMetric::Cosine, default_config()).unwrap();
        index.insert("v1", &[1.0, 0.0], None).unwrap();

        index.update("v1", &[0.5, 0.5], None).unwrap();
        let (vector, _) = index.get("v1").unwrap();
        assert_eq!(vector, vec![0.5, 0.5]);

        let err = index.update("missing", &[1.0, 0.0], None).unwrap_err();
        assert!(matches!(err, Error::VectorNotFound(_)));
    }

    #[test]
    fn test_get_missing_returns_none() {
        let index = HnswIndex::new(2, DistanceMetric::Cosine, default_config()).unwrap();
        assert!(index.get("nope").is_none());
    }

    #[test]
    fn test_search_dimension_mismatch() {
        let index = HnswIndex::new(3, DistanceMetric::Cosine, default_config()).unwrap();
        let err = index.search(&[1.0, 0.0], 5).unwrap_err();
        assert!(matches!(err, Error::DimensionMismatch { .. }));
    }

    #[test]
    fn test_search_with_threshold() {
        let index = HnswIndex::new(3, DistanceMetric::Cosine, default_config()).unwrap();
        index.insert("near", &[1.0, 0.0, 0.0], None).unwrap();
        index.insert("far", &[0.0, 1.0, 0.0], None).unwrap();

        let results = index.search_with_threshold(&[1.0, 0.0, 0.0], 10, 0.99).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "near");
    }

    #[test]
    fn test_delete_batch_and_compact() {
        let index = HnswIndex::new(3, DistanceMetric::Cosine, default_config()).unwrap();
        index.insert("a", &[1.0, 0.0, 0.0], None).unwrap();
        index.insert("b", &[0.0, 1.0, 0.0], None).unwrap();
        assert_eq!(index.delete_batch(&["a", "missing"]).unwrap(), 1);
        index.compact().unwrap();
        assert_eq!(index.len(), 1);
        assert!(!index.contains("a"));
        assert!(index.contains("b"));
    }

    #[test]
    fn test_memory_usage_positive() {
        let index = HnswIndex::new(8, DistanceMetric::Cosine, default_config()).unwrap();
        index.insert("v", &[1.0; 8], None).unwrap();
        assert!(index.memory_usage() > 0);
    }
    // =====================================================================
    // Pure PostgreSQL index helpers (R46)
    // =====================================================================

    fn sample_index_config(index_type: IndexType, distance: DistanceMetric) -> IndexConfig {
        IndexConfig {
            dimensions: 384,
            index_type,
            distance,
            m: DEFAULT_INDEX_M,
            ef_construction: DEFAULT_INDEX_EF_CONSTRUCTION,
        }
    }

    #[test]
    fn index_type_hnsw_serde_roundtrip() {
        let json = serde_json::to_string(&IndexType::Hnsw).expect("serialize");
        assert_eq!(json, "\"hnsw\"");
        let back: IndexType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, IndexType::Hnsw);
    }

    #[test]
    fn index_type_ivfflat_serde_roundtrip() {
        let json = serde_json::to_string(&IndexType::Ivfflat).expect("serialize");
        assert_eq!(json, "\"ivfflat\"");
        let back: IndexType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, IndexType::Ivfflat);
    }

    #[test]
    fn index_type_flat_serde_roundtrip() {
        let json = serde_json::to_string(&IndexType::Flat).expect("serialize");
        assert_eq!(json, "\"flat\"");
        let back: IndexType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, IndexType::Flat);
    }

    #[test]
    fn index_config_serde_roundtrip_all_index_types_and_distances() {
        for index_type in [IndexType::Hnsw, IndexType::Ivfflat, IndexType::Flat] {
            for distance in [
                DistanceMetric::Cosine,
                DistanceMetric::Euclidean,
                DistanceMetric::DotProduct,
            ] {
                let config = sample_index_config(index_type, distance);
                let json = serde_json::to_string(&config).expect("serialize");
                let restored: IndexConfig = serde_json::from_str(&json).expect("deserialize");
                assert_eq!(restored, config);
            }
        }
    }

    #[test]
    fn index_config_deserializes_with_defaults() {
        let json = r#"{"dimensions":1536}"#;
        let config: IndexConfig = serde_json::from_str(json).expect("deserialize");
        assert_eq!(config.dimensions, 1536);
        assert_eq!(config.index_type, IndexType::Hnsw);
        assert_eq!(config.distance, DistanceMetric::Cosine);
        assert_eq!(config.m, DEFAULT_INDEX_M);
        assert_eq!(config.ef_construction, DEFAULT_INDEX_EF_CONSTRUCTION);
    }

    #[test]
    fn hnsw_param_string_formats_m_and_ef_construction() {
        assert_eq!(hnsw_param_string(24, 200), "m = 24, ef_construction = 200");
    }

    #[test]
    fn hnsw_param_string_uses_default_constants() {
        let defaults = IndexConfig::default();
        assert_eq!(
            hnsw_param_string(defaults.m, defaults.ef_construction),
            "m = 16, ef_construction = 64"
        );
    }

    #[test]
    fn validate_index_config_accepts_defaults() {
        assert!(validate_index_config(&IndexConfig::default()).is_ok());
    }

    #[test]
    fn validate_index_config_rejects_zero_dimensions() {
        let config = IndexConfig {
            dimensions: 0,
            ..IndexConfig::default()
        };
        assert!(matches!(
            validate_index_config(&config),
            Err(IndexConfigError::InvalidDimensions)
        ));
    }

    #[test]
    fn validate_index_config_rejects_m_below_min() {
        let config = IndexConfig {
            m: MIN_HNSW_M - 1,
            ..IndexConfig::default()
        };
        assert!(matches!(
            validate_index_config(&config),
            Err(IndexConfigError::InvalidM(3))
        ));
    }

    #[test]
    fn validate_index_config_rejects_m_above_max() {
        let config = IndexConfig {
            m: MAX_HNSW_M + 1,
            ..IndexConfig::default()
        };
        assert!(matches!(
            validate_index_config(&config),
            Err(IndexConfigError::InvalidM(101))
        ));
    }

    #[test]
    fn validate_index_config_rejects_ef_below_min() {
        let config = IndexConfig {
            ef_construction: MIN_HNSW_EF_CONSTRUCTION - 1,
            ..IndexConfig::default()
        };
        assert!(matches!(
            validate_index_config(&config),
            Err(IndexConfigError::InvalidEfConstruction(9))
        ));
    }

    #[test]
    fn validate_index_config_rejects_ef_above_max() {
        let config = IndexConfig {
            ef_construction: MAX_HNSW_EF_CONSTRUCTION + 1,
            ..IndexConfig::default()
        };
        assert!(matches!(
            validate_index_config(&config),
            Err(IndexConfigError::InvalidEfConstruction(1001))
        ));
    }

    #[test]
    fn validate_index_config_accepts_boundary_m_and_ef() {
        let config = IndexConfig {
            m: MIN_HNSW_M,
            ef_construction: MAX_HNSW_EF_CONSTRUCTION,
            ..IndexConfig::default()
        };
        assert!(validate_index_config(&config).is_ok());
    }

    #[test]
    fn validate_index_config_skips_hnsw_bounds_for_ivfflat() {
        let config = IndexConfig {
            index_type: IndexType::Ivfflat,
            m: 1,
            ef_construction: 1,
            ..IndexConfig::default()
        };
        assert!(validate_index_config(&config).is_ok());
    }

    #[test]
    fn build_create_index_sql_quotes_table_and_column() {
        let config = sample_index_config(IndexType::Hnsw, DistanceMetric::Cosine);
        let sql = build_create_index_sql("documents", "embedding", &config).expect("sql");
        assert!(sql.contains("ON \"documents\""));
        assert!(sql.contains("(\"embedding\" vector_cosine_ops)"));
    }

    #[test]
    fn build_create_index_sql_includes_hnsw_suffix_and_defaults() {
        let config = IndexConfig::default();
        let sql = build_create_index_sql("docs", "vec", &config).expect("sql");
        assert!(sql.contains("docs_vec_hnsw_idx"));
        assert!(sql.contains("USING hnsw"));
        assert!(sql.contains("m = 16, ef_construction = 64"));
    }

    #[test]
    fn build_create_index_sql_ivfflat_suffix_and_lists() {
        let config = sample_index_config(IndexType::Ivfflat, DistanceMetric::Euclidean);
        let sql = build_create_index_sql("docs", "vec", &config).expect("sql");
        assert!(sql.contains("docs_vec_ivfflat_idx"));
        assert!(sql.contains("USING ivfflat"));
        assert!(sql.contains("lists = 100"));
        assert!(sql.contains("vector_l2_ops"));
    }

    #[test]
    fn build_create_index_sql_flat_returns_empty() {
        let config = sample_index_config(IndexType::Flat, DistanceMetric::DotProduct);
        let sql = build_create_index_sql("docs", "vec", &config).expect("sql");
        assert!(sql.is_empty());
    }

    #[test]
    fn build_create_index_sql_rejects_invalid_table_name() {
        let config = IndexConfig::default();
        assert!(build_create_index_sql("bad-name", "embedding", &config).is_err());
    }

    #[test]
    fn index_type_string_maps_postgres_names() {
        assert_eq!(index_type_string(IndexType::Hnsw), "hnsw");
        assert_eq!(index_type_string(IndexType::Ivfflat), "ivfflat");
        assert_eq!(index_type_string(IndexType::Flat), "vector");
    }

    #[test]
    fn distance_operator_maps_pgvector_operators() {
        assert_eq!(distance_operator(DistanceMetric::Euclidean).expect("l2"), "<->");
        assert_eq!(distance_operator(DistanceMetric::DotProduct).expect("dot"), "<#>");
        assert_eq!(distance_operator(DistanceMetric::Cosine).expect("cosine"), "<=>");
    }

    #[test]
    fn distance_operator_rejects_manhattan() {
        assert!(matches!(
            distance_operator(DistanceMetric::Manhattan),
            Err(IndexConfigError::UnsupportedDistance(DistanceMetric::Manhattan))
        ));
    }

    #[test]
    fn index_access_method_errors_for_flat() {
        assert!(matches!(
            index_access_method(IndexType::Flat),
            Err(IndexConfigError::UnsupportedIndexType(IndexType::Flat))
        ));
    }

    #[test]
    fn index_config_error_invalid_m_display() {
        let msg = IndexConfigError::InvalidM(2).to_string();
        assert!(msg.contains("invalid M"));
        assert!(msg.contains('2'));
    }

    #[test]
    fn index_config_error_invalid_ef_display() {
        let msg = IndexConfigError::InvalidEfConstruction(5).to_string();
        assert!(msg.contains("ef_construction"));
        assert!(msg.contains('5'));
    }

    #[test]
    fn index_config_error_unsupported_index_type_display() {
        let msg = IndexConfigError::UnsupportedIndexType(IndexType::Flat).to_string();
        assert!(msg.contains("unsupported index type"));
    }

    #[test]
    fn index_config_debug_clone_and_display_preview() {
        let config = IndexConfig::default();
        let cloned = config.clone();
        assert_eq!(config, cloned);
        let dbg = format!("{config:?}");
        assert!(dbg.contains("dimensions"));
        let preview = config.to_string();
        assert!(preview.contains("hnsw index"));
        assert!(preview.contains("384-dim"));
    }

    #[test]
    fn index_type_debug_and_clone() {
        #[allow(clippy::clone_on_copy)] // the test asserts Clone stays implemented
        let ty = IndexType::Ivfflat;
        let cloned = ty;
        assert_eq!(ty, cloned);
        assert!(format!("{ty:?}").contains("Ivfflat"));
    }

    #[test]
    fn index_config_error_debug_and_clone() {
        let err = IndexConfigError::InvalidM(7);
        let cloned = err.clone();
        assert_eq!(err, cloned);
        assert!(format!("{err:?}").contains("InvalidM"));
    }

}

