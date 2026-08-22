//! LanceDB Vector Store Implementation
//!
//! LanceDB is a serverless, embedded vector database that requires no
//! separate server process. Data is stored locally in a directory.
//!
//! Connection path resolution, configuration, and query predicate helpers are available under
//! `--features postgres`. The live store requires `--features lancedb`.

use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};

/// Default LanceDB storage path.
pub const DEFAULT_LANCEDB_PATH: &str = "./data/lancedb";

/// Default embedding dimensions (BGE-small).
pub const DEFAULT_VECTOR_DIMENSIONS: usize = 384;

/// Returns the default LanceDB path.
pub fn default_lancedb_path() -> String {
    DEFAULT_LANCEDB_PATH.to_string()
}

/// Returns the default vector dimensions.
pub fn default_vector_dimensions() -> usize {
    DEFAULT_VECTOR_DIMENSIONS
}

/// Resolve a LanceDB path from an explicit override, `LANCEDB_PATH`, or [`default_lancedb_path`].
pub fn resolve_lancedb_path(override_path: Option<&str>) -> String {
    if let Some(path) = override_path {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    std::env::var("LANCEDB_PATH").unwrap_or_else(|_| default_lancedb_path())
}

/// Configuration for a LanceDB-backed store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LanceDBConfig {
    #[serde(default = "default_lancedb_path")]
    pub path: String,
    #[serde(default = "default_vector_dimensions")]
    pub default_dimensions: usize,
}

impl Default for LanceDBConfig {
    fn default() -> Self {
        Self {
            path: default_lancedb_path(),
            default_dimensions: default_vector_dimensions(),
        }
    }
}

/// Validates a LanceDB storage path.
pub fn validate_lancedb_path(path: &str) -> Result<()> {
    if path.trim().is_empty() {
        return Err(AppError::Configuration(
            "empty lancedb path".to_string(),
        ));
    }
    Ok(())
}

/// Validates a collection/table name for LanceDB.
pub fn validate_collection_name(name: &str) -> std::result::Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("collection name must not be empty".to_string());
    }
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return Err("collection name must start with a letter or underscore".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("collection name may only contain ASCII letters, digits, and underscores".to_string());
    }
    Ok(())
}

/// Validates search parameters shared by vector queries.
pub fn validate_search_params(limit: usize, threshold: f32) -> std::result::Result<(), String> {
    if limit == 0 {
        return Err("limit must be greater than zero".to_string());
    }
    if !(0.0..=1.0).contains(&threshold) {
        return Err("threshold must be between 0.0 and 1.0".to_string());
    }
    Ok(())
}

/// Serializes document tags for LanceDB string storage.
pub fn serialize_metadata_tags(tags: &[String]) -> String {
    tags.join(",")
}

/// Deserializes comma-separated tags from LanceDB storage.
pub fn deserialize_metadata_tags(raw: &str) -> Vec<String> {
    raw.split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Converts cosine distance to a similarity score.
pub fn cosine_distance_to_score(distance: f32) -> f32 {
    1.0 - distance
}

/// Builds a SQL-like IN predicate for deleting documents by ID.
pub fn build_delete_predicate(id_field: &str, ids: &[String]) -> std::result::Result<String, String> {
    if ids.is_empty() {
        return Err("ids must not be empty".to_string());
    }
    let id_list = ids
        .iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!("{id_field} IN ({id_list})"))
}

/// Builds a SQL-like equality predicate for fetching a document by ID.
pub fn build_get_predicate(id_field: &str, id: &str) -> String {
    format!("{id_field} = '{}'", id.replace('\'', "''"))
}

/// Arrow schema field names for LanceDB tables.
pub mod schema {
    pub const ID: &str = "id";
    pub const CONTENT: &str = "content";
    pub const VECTOR: &str = "vector";
    pub const METADATA_TITLE: &str = "metadata_title";
    pub const METADATA_SOURCE: &str = "metadata_source";
    pub const METADATA_CREATED_AT: &str = "metadata_created_at";
    pub const METADATA_TAGS: &str = "metadata_tags";
}

#[cfg(feature = "lancedb")]
use crate::vectorstore::{CollectionInfo, CollectionStats, VectorStore};
#[cfg(feature = "lancedb")]
use ares_types::types::{Document, DocumentMetadata, SearchResult};
#[cfg(feature = "lancedb")]
use async_trait::async_trait;
#[cfg(feature = "lancedb")]
use lancedb::connection::Connection;
#[cfg(feature = "lancedb")]
use lancedb::query::{ExecutableQuery, QueryBase};
#[cfg(feature = "lancedb")]
use lancedb::{arrow::arrow_array, DistanceType};
#[cfg(feature = "lancedb")]
use parking_lot::RwLock;
#[cfg(feature = "lancedb")]
use std::collections::HashMap;
#[cfg(feature = "lancedb")]
use std::sync::Arc;
#[cfg(feature = "lancedb")]
use tracing::{debug, instrument, warn};

/// LanceDB vector store implementation.
///
/// Stores vectors in a local directory with DiskANN-based indexing.
/// No external server required - data is stored directly on disk.
#[cfg(feature = "lancedb")]
pub struct LanceDBStore {
    /// Database connection.
    connection: Connection,
    /// Path to the database directory.
    path: String,
    /// Cache of collection dimensions (collection_name -> dimensions).
    /// This avoids querying the table schema repeatedly.
    dimensions_cache: Arc<RwLock<HashMap<String, usize>>>,
}

#[cfg(feature = "lancedb")]
impl LanceDBStore {
    /// Create a new LanceDB store at the given path.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the LanceDB storage directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be established.
    #[instrument(skip_all, fields(path = %path))]
    pub async fn new(path: &str) -> Result<Self> {
        validate_lancedb_path(path)?;
        debug!("Connecting to LanceDB at {}", path);

        // Ensure the directory exists
        if let Err(e) = tokio::fs::create_dir_all(path).await {
            return Err(AppError::Database(format!(
                "Failed to create LanceDB directory: {}",
                e
            )));
        }

        let connection = lancedb::connect(path)
            .execute()
            .await
            .map_err(|e| AppError::Database(format!("Failed to connect to LanceDB: {}", e)))?;

        Ok(Self {
            connection,
            path: path.to_string(),
            dimensions_cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }


    /// Create from a [`LanceDBConfig`].
    pub async fn from_config(config: &LanceDBConfig) -> Result<Self> {
        Self::new(&config.path).await
    }

    /// Convert a Document to Arrow RecordBatch for insertion.
    fn documents_to_record_batch(
        &self,
        documents: &[Document],
        dimensions: usize,
    ) -> Result<arrow_array::RecordBatch> {
        use arrow_array::builder::{FixedSizeListBuilder, Float32Builder, StringBuilder};
        use arrow_array::types::Float32Type;
        use arrow_array::Array;
        use lancedb::arrow::arrow_schema::{DataType, Field, Schema};
        use std::sync::Arc as StdArc;

        let num_docs = documents.len();

        // Create builders
        let mut id_builder = StringBuilder::with_capacity(num_docs, num_docs * 64);
        let mut content_builder = StringBuilder::with_capacity(num_docs, num_docs * 1024);
        let mut vector_builder =
            FixedSizeListBuilder::new(Float32Builder::new(), dimensions as i32);
        let mut title_builder = StringBuilder::with_capacity(num_docs, num_docs * 128);
        let mut source_builder = StringBuilder::with_capacity(num_docs, num_docs * 256);
        let mut created_at_builder = StringBuilder::with_capacity(num_docs, num_docs * 32);
        let mut tags_builder = StringBuilder::with_capacity(num_docs, num_docs * 128);

        for doc in documents {
            id_builder.append_value(&doc.id);
            content_builder.append_value(&doc.content);

            // Append vector
            if let Some(ref embedding) = doc.embedding {
                if embedding.len() != dimensions {
                    return Err(AppError::InvalidInput(format!(
                        "Document '{}' has embedding of size {} but collection expects {}",
                        doc.id,
                        embedding.len(),
                        dimensions
                    )));
                }
                for &value in embedding {
                    vector_builder.values().append_value(value);
                }
                vector_builder.append(true);
            } else {
                return Err(AppError::InvalidInput(format!(
                    "Document '{}' is missing embedding",
                    doc.id
                )));
            }

            // Metadata
            title_builder.append_value(&doc.metadata.title);
            source_builder.append_value(&doc.metadata.source);
            created_at_builder.append_value(doc.metadata.created_at.to_rfc3339());
            tags_builder.append_value(serialize_metadata_tags(&doc.metadata.tags));
        }

        // Build arrays
        let id_array = StdArc::new(id_builder.finish()) as StdArc<dyn Array>;
        let content_array = StdArc::new(content_builder.finish()) as StdArc<dyn Array>;
        let vector_array = StdArc::new(vector_builder.finish()) as StdArc<dyn Array>;
        let title_array = StdArc::new(title_builder.finish()) as StdArc<dyn Array>;
        let source_array = StdArc::new(source_builder.finish()) as StdArc<dyn Array>;
        let created_at_array = StdArc::new(created_at_builder.finish()) as StdArc<dyn Array>;
        let tags_array = StdArc::new(tags_builder.finish()) as StdArc<dyn Array>;

        // Create schema
        let schema = Schema::new(vec![
            Field::new(schema::ID, DataType::Utf8, false),
            Field::new(schema::CONTENT, DataType::Utf8, false),
            Field::new(
                schema::VECTOR,
                DataType::FixedSizeList(
                    StdArc::new(Field::new("item", DataType::Float32, true)),
                    dimensions as i32,
                ),
                false,
            ),
            Field::new(schema::METADATA_TITLE, DataType::Utf8, true),
            Field::new(schema::METADATA_SOURCE, DataType::Utf8, true),
            Field::new(schema::METADATA_CREATED_AT, DataType::Utf8, true),
            Field::new(schema::METADATA_TAGS, DataType::Utf8, true),
        ]);

        arrow_array::RecordBatch::try_new(
            StdArc::new(schema),
            vec![
                id_array,
                content_array,
                vector_array,
                title_array,
                source_array,
                created_at_array,
                tags_array,
            ],
        )
        .map_err(|e| AppError::Database(format!("Failed to create RecordBatch: {}", e)))
    }

    /// Get cached dimensions for a collection.
    fn get_cached_dimensions(&self, collection: &str) -> Option<usize> {
        self.dimensions_cache.read().get(collection).copied()
    }

    /// Cache dimensions for a collection.
    fn cache_dimensions(&self, collection: &str, dimensions: usize) {
        self.dimensions_cache
            .write()
            .insert(collection.to_string(), dimensions);
    }

    /// Get dimensions from table schema.
    async fn get_dimensions_from_table(&self, collection: &str) -> Result<usize> {
        // Check cache first
        if let Some(dims) = self.get_cached_dimensions(collection) {
            return Ok(dims);
        }

        // Query the table to get schema
        let table = self
            .connection
            .open_table(collection)
            .execute()
            .await
            .map_err(|e| {
                AppError::NotFound(format!("Collection '{}' not found: {}", collection, e))
            })?;

        // Get schema from a small query
        let results = table
            .query()
            .limit(1)
            .execute()
            .await
            .map_err(|e| AppError::Database(format!("Failed to query table schema: {}", e)))?;

        use futures::TryStreamExt;
        let batches: Vec<_> = results
            .try_collect()
            .await
            .map_err(|e| AppError::Database(format!("Failed to collect schema: {}", e)))?;

        if batches.is_empty() {
            // Table exists but is empty - check schema directly
            // For now, return error - we'd need schema metadata
            return Err(AppError::Database(format!(
                "Collection '{}' is empty, cannot determine dimensions",
                collection
            )));
        }

        let schema = batches[0].schema();
        for field in schema.fields() {
            if field.name() == schema::VECTOR {
                if let lancedb::arrow::arrow_schema::DataType::FixedSizeList(_, size) =
                    field.data_type()
                {
                    let dims = *size as usize;
                    self.cache_dimensions(collection, dims);
                    return Ok(dims);
                }
            }
        }

        Err(AppError::Database(format!(
            "Could not determine dimensions for collection '{}'",
            collection
        )))
    }
}

#[cfg(feature = "lancedb")]
#[async_trait]
impl VectorStore for LanceDBStore {
    fn provider_name(&self) -> &'static str {
        "lancedb"
    }

    #[instrument(skip(self), fields(collection = %name, dimensions = %dimensions))]
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        use arrow_array::builder::{FixedSizeListBuilder, Float32Builder, StringBuilder};
        use arrow_array::Array;
        use lancedb::arrow::arrow_schema::{DataType, Field, Schema};
        use std::sync::Arc as StdArc;

        debug!(
            "Creating collection '{}' with {} dimensions",
            name, dimensions
        );

        // Check if table already exists
        let tables = self
            .connection
            .table_names()
            .execute()
            .await
            .map_err(|e| AppError::Database(format!("Failed to list tables: {}", e)))?;

        if tables.contains(&name.to_string()) {
            return Err(AppError::InvalidInput(format!(
                "Collection '{}' already exists",
                name
            )));
        }

        // Create schema with empty record batch to define table structure
        let schema = Schema::new(vec![
            Field::new(schema::ID, DataType::Utf8, false),
            Field::new(schema::CONTENT, DataType::Utf8, false),
            Field::new(
                schema::VECTOR,
                DataType::FixedSizeList(
                    StdArc::new(Field::new("item", DataType::Float32, true)),
                    dimensions as i32,
                ),
                false,
            ),
            Field::new(schema::METADATA_TITLE, DataType::Utf8, true),
            Field::new(schema::METADATA_SOURCE, DataType::Utf8, true),
            Field::new(schema::METADATA_CREATED_AT, DataType::Utf8, true),
            Field::new(schema::METADATA_TAGS, DataType::Utf8, true),
        ]);

        // Create builders for empty table
        let mut id_builder = StringBuilder::new();
        let mut content_builder = StringBuilder::new();
        let mut vector_builder = FixedSizeListBuilder::new(Float32Builder::new(), dimensions as i32);
        let mut title_builder = StringBuilder::new();
        let mut source_builder = StringBuilder::new();
        let mut created_at_builder = StringBuilder::new();
        let mut tags_builder = StringBuilder::new();

        let batch = arrow_array::RecordBatch::try_new(
            StdArc::new(schema),
            vec![
                StdArc::new(id_builder.finish()) as StdArc<dyn Array>,
                StdArc::new(content_builder.finish()) as StdArc<dyn Array>,
                StdArc::new(vector_builder.finish()) as StdArc<dyn Array>,
                StdArc::new(title_builder.finish()) as StdArc<dyn Array>,
                StdArc::new(source_builder.finish()) as StdArc<dyn Array>,
                StdArc::new(created_at_builder.finish()) as StdArc<dyn Array>,
                StdArc::new(tags_builder.finish()) as StdArc<dyn Array>,
            ],
        )
        .map_err(|e| AppError::Database(format!("Failed to create schema batch: {}", e)))?;

        self.connection
            .create_empty_table(name, StdArc::new(batch.schema().as_ref().clone()))
            .execute()
            .await
            .map_err(|e| AppError::Database(format!("Failed to create table: {}", e)))?;

        // Cache dimensions
        self.cache_dimensions(name, dimensions);

        debug!("Created collection '{}'", name);
        Ok(())
    }

    #[instrument(skip(self), fields(collection = %name))]
    async fn delete_collection(&self, name: &str) -> Result<()> {
        debug!("Deleting collection '{}'", name);

        self.connection
            .drop_table(name, &[])
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete collection: {}", e)))?;

        // Remove from cache
        self.dimensions_cache.write().remove(name);

        debug!("Deleted collection '{}'", name);
        Ok(())
    }

    #[instrument(skip(self))]
    async fn list_collections(&self) -> Result<Vec<CollectionInfo>> {
        let table_names = self
            .connection
            .table_names()
            .execute()
            .await
            .map_err(|e| AppError::Database(format!("Failed to list tables: {}", e)))?;

        let mut collections = Vec::new();

        for name in table_names {
            // Try to get stats for each table
            match self.collection_stats(&name).await {
                Ok(stats) => {
                    collections.push(CollectionInfo {
                        name: stats.name,
                        document_count: stats.document_count,
                        dimensions: stats.dimensions,
                    });
                }
                Err(e) => {
                    warn!("Failed to get stats for collection '{}': {}", name, e);
                    // Include anyway with unknown values
                    collections.push(CollectionInfo {
                        name,
                        document_count: 0,
                        dimensions: 0,
                    });
                }
            }
        }

        Ok(collections)
    }

    #[instrument(skip(self), fields(collection = %name))]
    async fn collection_exists(&self, name: &str) -> Result<bool> {
        let tables = self
            .connection
            .table_names()
            .execute()
            .await
            .map_err(|e| AppError::Database(format!("Failed to list tables: {}", e)))?;

        Ok(tables.contains(&name.to_string()))
    }

    #[instrument(skip(self), fields(collection = %name))]
    async fn collection_stats(&self, name: &str) -> Result<CollectionStats> {
        let table = self
            .connection
            .open_table(name)
            .execute()
            .await
            .map_err(|e| AppError::NotFound(format!("Collection '{}' not found: {}", name, e)))?;

        let count = table
            .count_rows(None)
            .await
            .map_err(|e| AppError::Database(format!("Failed to count rows: {}", e)))?;

        let dimensions = self.get_dimensions_from_table(name).await.unwrap_or(0);

        Ok(CollectionStats {
            name: name.to_string(),
            document_count: count,
            dimensions,
            index_size_bytes: None, // LanceDB doesn't expose this easily
            distance_metric: "cosine".to_string(),
        })
    }

    #[instrument(skip(self, documents), fields(collection = %collection, doc_count = documents.len()))]
    async fn upsert(&self, collection: &str, documents: &[Document]) -> Result<usize> {
        if documents.is_empty() {
            return Ok(0);
        }

        debug!(
            "Upserting {} documents to '{}'",
            documents.len(),
            collection
        );

        let dimensions = self.get_dimensions_from_table(collection).await?;
        let batch = self.documents_to_record_batch(documents, dimensions)?;

        let table = self
            .connection
            .open_table(collection)
            .execute()
            .await
            .map_err(|e| {
                AppError::NotFound(format!("Collection '{}' not found: {}", collection, e))
            })?;

        // Use merge insert (upsert) based on ID. The 0.37.1 builder methods
        // return `&mut Self`, so build modifiers as separate statements, then
        // execute with a boxed RecordBatchReader.
        use arrow_array::RecordBatchIterator;
        let schema = batch.schema();
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
        let mut merge = table.merge_insert(&[schema::ID]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        merge
            .execute(reader)
            .await
            .map_err(|e| AppError::Database(format!("Failed to upsert: {}", e)))?;

        debug!("Upserted {} documents", documents.len());
        Ok(documents.len())
    }

    #[instrument(skip(self, embedding), fields(collection = %collection, limit = %limit, threshold = %threshold))]
    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>> {
        validate_search_params(limit, threshold).map_err(|e| AppError::InvalidInput(e))?;
        debug!(
            "Searching '{}' with threshold {} and limit {}",
            collection, threshold, limit
        );

        let table = self
            .connection
            .open_table(collection)
            .execute()
            .await
            .map_err(|e| {
                AppError::NotFound(format!("Collection '{}' not found: {}", collection, e))
            })?;

        let query_vec: Vec<f32> = embedding.to_vec();

        let results = table
            .vector_search(query_vec)
            .map_err(|e| AppError::Database(format!("Failed to create search query: {}", e)))?
            .distance_type(DistanceType::Cosine)
            .limit(limit)
            .execute()
            .await
            .map_err(|e| AppError::Database(format!("Failed to execute search: {}", e)))?;

        use futures::TryStreamExt;
        let batches: Vec<_> = results
            .try_collect()
            .await
            .map_err(|e| AppError::Database(format!("Failed to collect results: {}", e)))?;

        let mut search_results = Vec::new();

        for batch in batches {
            let id_col = batch
                .column_by_name(schema::ID)
                .ok_or_else(|| AppError::Database("Missing ID column".to_string()))?;
            let content_col = batch
                .column_by_name(schema::CONTENT)
                .ok_or_else(|| AppError::Database("Missing content column".to_string()))?;
            let title_col = batch.column_by_name(schema::METADATA_TITLE);
            let source_col = batch.column_by_name(schema::METADATA_SOURCE);
            let created_at_col = batch.column_by_name(schema::METADATA_CREATED_AT);
            let tags_col = batch.column_by_name(schema::METADATA_TAGS);
            let distance_col = batch.column_by_name("_distance");

            let id_array = id_col
                .as_any()
                .downcast_ref::<arrow_array::StringArray>()
                .ok_or_else(|| AppError::Database("ID column is not string".to_string()))?;
            let content_array = content_col
                .as_any()
                .downcast_ref::<arrow_array::StringArray>()
                .ok_or_else(|| AppError::Database("Content column is not string".to_string()))?;

            for i in 0..batch.num_rows() {
                // Convert distance to similarity score (cosine distance to similarity)
                let distance = distance_col
                    .and_then(|col| {
                        col.as_any()
                            .downcast_ref::<arrow_array::Float32Array>()
                            .map(|arr| arr.value(i))
                    })
                    .unwrap_or(0.0);

                let score = cosine_distance_to_score(distance);

                // Skip if below threshold
                if score < threshold {
                    continue;
                }

                let id = id_array.value(i).to_string();
                let content = content_array.value(i).to_string();

                // Extract metadata
                let title = title_col
                    .and_then(|col| {
                        col.as_any()
                            .downcast_ref::<arrow_array::StringArray>()
                            .map(|arr| arr.value(i).to_string())
                    })
                    .unwrap_or_default();

                let source = source_col
                    .and_then(|col| {
                        col.as_any()
                            .downcast_ref::<arrow_array::StringArray>()
                            .map(|arr| arr.value(i).to_string())
                    })
                    .unwrap_or_default();

                let created_at = created_at_col
                    .and_then(|col| {
                        col.as_any()
                            .downcast_ref::<arrow_array::StringArray>()
                            .and_then(|arr| {
                                chrono::DateTime::parse_from_rfc3339(arr.value(i))
                                    .map(|dt| dt.with_timezone(&chrono::Utc))
                                    .ok()
                            })
                    })
                    .unwrap_or_else(chrono::Utc::now);

                let tags: Vec<String> = tags_col
                    .and_then(|col| {
                        col.as_any()
                            .downcast_ref::<arrow_array::StringArray>()
                            .map(|arr| deserialize_metadata_tags(arr.value(i)))
                    })
                    .unwrap_or_default();

                search_results.push(SearchResult {
                    document: Document {
                        id,
                        content,
                        metadata: DocumentMetadata {
                            title,
                            source,
                            created_at,
                            tags,
                        },
                        embedding: None, // Don't return embeddings
                    },
                    score,
                });
            }
        }

        // Sort by score descending (should already be sorted, but ensure)
        search_results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        debug!("Found {} results", search_results.len());
        Ok(search_results)
    }

    #[instrument(skip(self, ids), fields(collection = %collection, count = ids.len()))]
    async fn delete(&self, collection: &str, ids: &[String]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }

        debug!("Deleting {} documents from '{}'", ids.len(), collection);

        let table = self
            .connection
            .open_table(collection)
            .execute()
            .await
            .map_err(|e| {
                AppError::NotFound(format!("Collection '{}' not found: {}", collection, e))
            })?;

        // Build WHERE clause for deletion
        let id_list = ids
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");

        let predicate = format!("{} IN ({})", schema::ID, id_list);

        table
            .delete(&predicate)
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete: {}", e)))?;

        debug!("Deleted {} documents", ids.len());
        Ok(ids.len())
    }

    #[instrument(skip(self), fields(collection = %collection, id = %id))]
    async fn get(&self, collection: &str, id: &str) -> Result<Option<Document>> {
        let table = self
            .connection
            .open_table(collection)
            .execute()
            .await
            .map_err(|e| {
                AppError::NotFound(format!("Collection '{}' not found: {}", collection, e))
            })?;

        let predicate = build_get_predicate(schema::ID, id);

        let results = table
            .query()
            .only_if(predicate)
            .limit(1)
            .execute()
            .await
            .map_err(|e| AppError::Database(format!("Failed to query: {}", e)))?;

        use futures::TryStreamExt;
        let batches: Vec<_> = results
            .try_collect()
            .await
            .map_err(|e| AppError::Database(format!("Failed to collect results: {}", e)))?;

        if batches.is_empty() || batches[0].num_rows() == 0 {
            return Ok(None);
        }

        let batch = &batches[0];
        let id_col = batch
            .column_by_name(schema::ID)
            .ok_or_else(|| AppError::Database("Missing ID column".to_string()))?;
        let content_col = batch
            .column_by_name(schema::CONTENT)
            .ok_or_else(|| AppError::Database("Missing content column".to_string()))?;

        let id_array = id_col
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .ok_or_else(|| AppError::Database("ID column is not string".to_string()))?;
        let content_array = content_col
            .as_any()
            .downcast_ref::<arrow_array::StringArray>()
            .ok_or_else(|| AppError::Database("Content column is not string".to_string()))?;

        let title = batch
            .column_by_name(schema::METADATA_TITLE)
            .and_then(|col| {
                col.as_any()
                    .downcast_ref::<arrow_array::StringArray>()
                    .map(|arr| arr.value(0).to_string())
            })
            .unwrap_or_default();

        let source = batch
            .column_by_name(schema::METADATA_SOURCE)
            .and_then(|col| {
                col.as_any()
                    .downcast_ref::<arrow_array::StringArray>()
                    .map(|arr| arr.value(0).to_string())
            })
            .unwrap_or_default();

        let created_at = batch
            .column_by_name(schema::METADATA_CREATED_AT)
            .and_then(|col| {
                col.as_any()
                    .downcast_ref::<arrow_array::StringArray>()
                    .and_then(|arr| {
                        chrono::DateTime::parse_from_rfc3339(arr.value(0))
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                            .ok()
                    })
            })
            .unwrap_or_else(chrono::Utc::now);

        let tags: Vec<String> = batch
            .column_by_name(schema::METADATA_TAGS)
            .and_then(|col| {
                col.as_any()
                    .downcast_ref::<arrow_array::StringArray>()
                    .map(|arr| deserialize_metadata_tags(arr.value(0)))
            })
            .unwrap_or_default();

        Ok(Some(Document {
            id: id_array.value(0).to_string(),
            content: content_array.value(0).to_string(),
            metadata: DocumentMetadata {
                title,
                source,
                created_at,
                tags,
            },
            embedding: None,
        }))
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::AppError;
    #[cfg(feature = "lancedb")]
    use ares_types::types::{Document, DocumentMetadata};
    #[cfg(feature = "lancedb")]
    use chrono::Utc;

    // ── Connection logic ─────────────────────────────────────────────────

    #[test]
    fn default_lancedb_path_matches_constant() {
        assert_eq!(default_lancedb_path(), DEFAULT_LANCEDB_PATH);
    }

    #[test]
    fn resolve_lancedb_path_prefers_explicit_override() {
        std::env::remove_var("LANCEDB_PATH");
        assert_eq!(
            resolve_lancedb_path(Some("/tmp/lancedb")),
            "/tmp/lancedb"
        );
    }

    #[test]
    fn resolve_lancedb_path_trims_override() {
        std::env::remove_var("LANCEDB_PATH");
        assert_eq!(
            resolve_lancedb_path(Some("  /tmp/trimmed  ")),
            "/tmp/trimmed"
        );
    }

    #[test]
    fn resolve_lancedb_path_falls_back_to_default_when_env_missing() {
        std::env::remove_var("LANCEDB_PATH");
        assert_eq!(resolve_lancedb_path(None), default_lancedb_path());
    }

    #[test]
    fn resolve_lancedb_path_ignores_blank_override() {
        std::env::remove_var("LANCEDB_PATH");
        assert_eq!(resolve_lancedb_path(Some("   ")), default_lancedb_path());
    }

    #[test]
    fn validate_lancedb_path_rejects_empty_path() {
        let err = validate_lancedb_path("   ").unwrap_err();
        matches::assert_matches!(err, AppError::Configuration(msg) if msg.contains("empty"));
    }

    // ── Query building ───────────────────────────────────────────────────

    #[test]
    fn build_delete_predicate_formats_in_clause() {
        let predicate = build_delete_predicate(schema::ID, &["doc1".into(), "doc2".into()])
            .expect("predicate");
        assert_eq!(predicate, "id IN ('doc1', 'doc2')");
    }

    #[test]
    fn build_delete_predicate_escapes_single_quotes() {
        let predicate = build_delete_predicate(schema::ID, &["it's".into()])
            .expect("predicate");
        assert!(predicate.contains("it''s"));
    }

    #[test]
    fn build_delete_predicate_rejects_empty_ids() {
        assert!(build_delete_predicate(schema::ID, &[]).is_err());
    }

    #[test]
    fn build_get_predicate_formats_equality_clause() {
        let predicate = build_get_predicate(schema::ID, "doc1");
        assert_eq!(predicate, "id = 'doc1'");
    }

    #[test]
    fn serialize_and_deserialize_metadata_tags() {
        let tags = vec!["alpha".into(), "beta".into()];
        assert_eq!(serialize_metadata_tags(&tags), "alpha,beta");
        assert_eq!(deserialize_metadata_tags("alpha,beta"), tags);
    }

    #[test]
    fn cosine_distance_to_score_converts_distance() {
        assert!((cosine_distance_to_score(0.2) - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn validate_search_params_rejects_invalid_values() {
        assert!(validate_search_params(0, 0.5).is_err());
        assert!(validate_search_params(10, 1.2).is_err());
    }

    #[test]
    fn validate_collection_name_rejects_invalid() {
        assert!(validate_collection_name("").is_err());
        assert!(validate_collection_name("1bad").is_err());
    }

    // ── Serde: LanceDBConfig ─────────────────────────────────────────────

    #[test]
    fn lancedb_config_default_values() {
        let config = LanceDBConfig::default();
        assert_eq!(config.path, default_lancedb_path());
        assert_eq!(config.default_dimensions, DEFAULT_VECTOR_DIMENSIONS);
    }

    #[test]
    fn lancedb_config_serde_roundtrip() {
        let config = LanceDBConfig {
            path: "/var/data/lancedb".into(),
            default_dimensions: 1536,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: LanceDBConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, config);
    }

    #[test]
    fn lancedb_config_deserializes_with_defaults() {
        let json = r#"{"path":"/custom/lancedb"}"#;
        let config: LanceDBConfig = serde_json::from_str(json).expect("deserialize");
        assert_eq!(config.path, "/custom/lancedb");
        assert_eq!(config.default_dimensions, DEFAULT_VECTOR_DIMENSIONS);
    }

    // ── Error handling ───────────────────────────────────────────────────

    #[cfg(feature = "lancedb")]
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

    #[cfg(feature = "lancedb")]
    #[tokio::test]
    async fn new_rejects_empty_path() {
        let err = LanceDBStore::new("   ").await.err().unwrap();
        matches::assert_matches!(err, AppError::Configuration(_));
    }

    #[cfg(feature = "lancedb")]
    #[tokio::test]
    async fn from_config_validates_path() {
        let config = LanceDBConfig {
            path: "   ".into(),
            ..LanceDBConfig::default()
        };
        let err = LanceDBStore::from_config(&config).await.err().unwrap();
        matches::assert_matches!(err, AppError::Configuration(_));
    }

    #[cfg(feature = "lancedb")]
    mod integration {
        use super::*;
        use tempfile::TempDir;

        #[tokio::test]
        async fn test_lancedb_create_collection() {
            let tmp = TempDir::new().unwrap();
            let store = LanceDBStore::new(tmp.path().to_str().unwrap())
                .await
                .unwrap();

            store.create_collection("test", 384).await.unwrap();
            assert!(store.collection_exists("test").await.unwrap());
        }

        #[tokio::test]
        async fn test_lancedb_duplicate_collection_error() {
            let tmp = TempDir::new().unwrap();
            let store = LanceDBStore::new(tmp.path().to_str().unwrap())
                .await
                .unwrap();

            store.create_collection("test", 384).await.unwrap();
            let result = store.create_collection("test", 384).await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_lancedb_upsert_and_search() {
            let tmp = TempDir::new().unwrap();
            let store = LanceDBStore::new(tmp.path().to_str().unwrap())
                .await
                .unwrap();

            store.create_collection("test", 3).await.unwrap();

            let doc1 = create_test_document("doc1", "Hello world", vec![1.0, 0.0, 0.0]);
            let doc2 = create_test_document("doc2", "Goodbye world", vec![0.0, 1.0, 0.0]);
            let doc3 = create_test_document("doc3", "Hello again", vec![0.9, 0.1, 0.0]);

            store.upsert("test", &[doc1, doc2, doc3]).await.unwrap();

            let results = store
                .search("test", &[1.0, 0.0, 0.0], 10, 0.5)
                .await
                .unwrap();

            assert!(!results.is_empty());
            assert_eq!(results[0].document.id, "doc1");
        }

        #[tokio::test]
        async fn test_lancedb_delete() {
            let tmp = TempDir::new().unwrap();
            let store = LanceDBStore::new(tmp.path().to_str().unwrap())
                .await
                .unwrap();

            store.create_collection("test", 3).await.unwrap();

            let doc = create_test_document("doc1", "Test", vec![1.0, 0.0, 0.0]);
            store.upsert("test", &[doc]).await.unwrap();

            let stats = store.collection_stats("test").await.unwrap();
            assert_eq!(stats.document_count, 1);

            store.delete("test", &["doc1".to_string()]).await.unwrap();

            let stats = store.collection_stats("test").await.unwrap();
            assert_eq!(stats.document_count, 0);
        }

        #[tokio::test]
        async fn test_lancedb_get() {
            let tmp = TempDir::new().unwrap();
            let store = LanceDBStore::new(tmp.path().to_str().unwrap())
                .await
                .unwrap();

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
        async fn test_lancedb_list_collections() {
            let tmp = TempDir::new().unwrap();
            let store = LanceDBStore::new(tmp.path().to_str().unwrap())
                .await
                .unwrap();

            store.create_collection("col1", 384).await.unwrap();
            store.create_collection("col2", 768).await.unwrap();

            let collections = store.list_collections().await.unwrap();
            assert_eq!(collections.len(), 2);
        }
    }
}
