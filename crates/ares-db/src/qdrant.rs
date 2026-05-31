//! Qdrant vector database integration.
//!
//! Connection URL resolution, configuration, and query-building helpers are available under
//! `--features postgres`. The live client requires `--features qdrant`.

use ares_types::types::{AppError, Document, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Default Qdrant HTTP/gRPC URL.
pub const DEFAULT_QDRANT_URL: &str = "http://127.0.0.1:6334";

/// Default embedding dimensions (BGE-small).
pub const DEFAULT_VECTOR_DIMENSIONS: usize = 384;

/// Returns the default Qdrant URL.
pub fn default_qdrant_url() -> String {
    DEFAULT_QDRANT_URL.to_string()
}

/// Returns the default vector dimensions.
pub fn default_vector_dimensions() -> usize {
    DEFAULT_VECTOR_DIMENSIONS
}

/// Resolve a Qdrant URL from an explicit override, `QDRANT_URL`, or [`default_qdrant_url`].
pub fn resolve_qdrant_url(override_url: Option<&str>) -> String {
    if let Some(url) = override_url {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    std::env::var("QDRANT_URL").unwrap_or_else(|_| default_qdrant_url())
}

/// Resolve an optional Qdrant API key from override or `QDRANT_API_KEY`.
pub fn resolve_qdrant_api_key(override_key: Option<&str>) -> Option<String> {
    if let Some(key) = override_key {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    std::env::var("QDRANT_API_KEY")
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// Configuration for a Qdrant-backed store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QdrantConfig {
    #[serde(default = "default_qdrant_url")]
    pub url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_vector_dimensions")]
    pub default_dimensions: usize,
}

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            url: default_qdrant_url(),
            api_key: None,
            default_dimensions: default_vector_dimensions(),
        }
    }
}

/// Validates a Qdrant connection URL.
pub fn validate_qdrant_url(url: &str) -> Result<()> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(AppError::Configuration("empty qdrant url".to_string()));
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(AppError::Configuration(format!(
            "invalid qdrant url (expected http/https): {trimmed}"
        )));
    }
    Ok(())
}

/// Validates a collection name for Qdrant.
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

/// Describes how a point ID should be encoded for Qdrant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PointIdKind {
    Numeric(u64),
    Uuid(String),
}

/// Classifies a string ID as numeric or UUID for Qdrant point operations.
pub fn classify_point_id(id: &str) -> PointIdKind {
    if let Ok(num) = id.parse::<u64>() {
        PointIdKind::Numeric(num)
    } else {
        PointIdKind::Uuid(id.to_string())
    }
}

/// Builds JSON payload fields for a document upsert.
pub fn build_document_payload_fields(document: &Document) -> HashMap<String, serde_json::Value> {
    let mut payload = HashMap::new();
    payload.insert(
        "content".to_string(),
        serde_json::Value::String(document.content.clone()),
    );
    payload.insert(
        "title".to_string(),
        serde_json::Value::String(document.metadata.title.clone()),
    );
    payload.insert(
        "source".to_string(),
        serde_json::Value::String(document.metadata.source.clone()),
    );
    payload.insert(
        "created_at".to_string(),
        serde_json::Value::Number(document.metadata.created_at.timestamp().into()),
    );
    payload.insert(
        "tags".to_string(),
        serde_json::to_value(&document.metadata.tags).unwrap_or(serde_json::Value::Null),
    );
    payload
}

/// Returns the metadata filter field keys that will be applied to a search.
pub fn build_filter_field_keys(filters: &[(String, String)]) -> Vec<String> {
    filters.iter().map(|(field, _)| field.clone()).collect()
}

/// Describes a cosine search request for assertions and logging.
pub fn describe_search_request(
    collection: &str,
    limit: usize,
    threshold: f32,
    filters: &[(String, String)],
) -> std::result::Result<String, String> {
    validate_collection_name(collection)?;
    validate_search_params(limit, threshold)?;
    let filter_keys = build_filter_field_keys(filters);
    Ok(format!(
        "collection={collection} limit={limit} threshold={threshold} filters={filter_keys:?}"
    ))
}

#[cfg(feature = "qdrant")]
use ares_types::types::SearchResult;
#[cfg(feature = "qdrant")]
use async_trait::async_trait;
#[cfg(feature = "qdrant")]
use qdrant_client::{
    qdrant::{
        condition::ConditionOneOf, r#match::MatchValue, Condition, CreateCollectionBuilder,
        DeletePointsBuilder, Distance, FieldCondition, Filter, Match, PointId, PointStruct,
        SearchPointsBuilder, UpsertPointsBuilder, VectorParamsBuilder,
    },
    Qdrant,
};
#[cfg(feature = "qdrant")]
use super::vectorstore::{CollectionInfo, CollectionStats, VectorStore};

/// Qdrant vector store implementation.
///
/// Provides vector storage and similarity search using a Qdrant server.
/// Requires a running Qdrant instance.
#[cfg(feature = "qdrant")]
pub struct QdrantVectorStore {
    client: Qdrant,
}

#[cfg(feature = "qdrant")]
impl QdrantVectorStore {
    pub async fn new(url: String, api_key: Option<String>) -> Result<Self> {
        let client = if let Some(key) = api_key {
            Qdrant::from_url(&url)
                .api_key(key)
                .build()
                .map_err(|e| AppError::Database(format!("Failed to create Qdrant client: {}", e)))?
        } else {
            Qdrant::from_url(&url)
                .build()
                .map_err(|e| AppError::Database(format!("Failed to create Qdrant client: {}", e)))?
        };

        let qdrant = Self { client };
        // qdrant.initialize_collections().await?;

        Ok(qdrant)
    }

    #[allow(dead_code)]
    async fn initialize_collections(&self) -> Result<()> {
        let collection_name = "documents";

        // Check if collection exists
        let collections = self
            .client
            .list_collections()
            .await
            .map_err(|e| AppError::Database(format!("Failed to list collections: {}", e)))?;

        let exists = collections
            .collections
            .iter()
            .any(|c| c.name == collection_name);

        if !exists {
            // Create collection with 384-dimensional vectors (for BGE-small)
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(collection_name)
                        .vectors_config(VectorParamsBuilder::new(384, Distance::Cosine)),
                )
                .await
                .map_err(|e| AppError::Database(format!("Failed to create collection: {}", e)))?;
        }

        Ok(())
    }

    pub async fn upsert_document(&self, document: &Document) -> Result<()> {
        let collection_name = "documents";

        let embedding = document
            .embedding
            .as_ref()
            .ok_or_else(|| AppError::Database("Document missing embedding".to_string()))?;

        let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
        payload.insert("content".to_string(), document.content.clone().into());
        payload.insert("title".to_string(), document.metadata.title.clone().into());
        payload.insert(
            "source".to_string(),
            document.metadata.source.clone().into(),
        );
        payload.insert(
            "created_at".to_string(),
            document.metadata.created_at.timestamp().into(),
        );
        payload.insert(
            "tags".to_string(),
            serde_json::to_value(&document.metadata.tags)
                .unwrap_or(serde_json::Value::Null)
                .into(),
        );

        let point = PointStruct::new(document.id.clone(), embedding.clone(), payload);

        self.client
            .upsert_points(UpsertPointsBuilder::new(collection_name, vec![point]).wait(true))
            .await
            .map_err(|e| AppError::Database(format!("Failed to upsert point: {}", e)))?;

        Ok(())
    }

    /// Parse search results from Qdrant response.
    fn parse_search_results(
        &self,
        search_result: qdrant_client::qdrant::SearchResponse,
    ) -> Vec<SearchResult> {
        search_result
            .result
            .into_iter()
            .filter_map(|scored_point| {
                let payload = scored_point.payload;
                let content = payload.get("content")?.as_str()?.to_string();
                let title = payload.get("title")?.as_str()?.to_string();
                let source = payload.get("source")?.as_str()?.to_string();
                let created_at_ts = payload.get("created_at")?.as_integer()?;
                let tags: Vec<String> =
                    serde_json::from_value(payload.get("tags")?.clone().into()).ok()?;

                let id_str = match scored_point.id?.point_id_options? {
                    qdrant_client::qdrant::point_id::PointIdOptions::Num(num) => num.to_string(),
                    qdrant_client::qdrant::point_id::PointIdOptions::Uuid(uuid) => uuid,
                };
                Some(SearchResult {
                    document: Document {
                        id: id_str,
                        content,
                        metadata: crate::types::DocumentMetadata {
                            title,
                            source,
                            created_at: chrono::DateTime::from_timestamp(created_at_ts, 0)?,
                            tags,
                        },
                        embedding: None,
                    },
                    score: scored_point.score,
                })
            })
            .collect()
    }

    #[allow(dead_code)]
    pub async fn delete_document(&self, id: &str) -> Result<()> {
        self.delete("documents", &[id.to_string()]).await?;
        Ok(())
    }
}

// ============================================================================
// VectorStore Trait Implementation
// ============================================================================

#[cfg(feature = "qdrant")]
#[async_trait]
impl VectorStore for QdrantVectorStore {
    fn provider_name(&self) -> &'static str {
        "qdrant"
    }

    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        // Check if collection exists
        let collections = self
            .client
            .list_collections()
            .await
            .map_err(|e| AppError::Database(format!("Failed to list collections: {}", e)))?;

        let exists = collections.collections.iter().any(|c| c.name == name);

        if !exists {
            self.client
                .create_collection(CreateCollectionBuilder::new(name).vectors_config(
                    VectorParamsBuilder::new(dimensions as u64, Distance::Cosine),
                ))
                .await
                .map_err(|e| AppError::Database(format!("Failed to create collection: {}", e)))?;
        }

        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        self.client
            .delete_collection(name)
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete collection: {}", e)))?;
        Ok(())
    }

    async fn list_collections(&self) -> Result<Vec<CollectionInfo>> {
        let collections = self
            .client
            .list_collections()
            .await
            .map_err(|e| AppError::Database(format!("Failed to list collections: {}", e)))?;

        let mut result = Vec::new();
        for col in collections.collections {
            // Get collection info for each
            if let Ok(info) = self.client.collection_info(&col.name).await {
                if let Some(collection_info) = info.result {
                    let count = collection_info.points_count.unwrap_or(0) as usize;
                    let dims = collection_info
                        .config
                        .and_then(|c| c.params)
                        .and_then(|p| p.vectors_config)
                        .and_then(|v| match v.config {
                            Some(qdrant_client::qdrant::vectors_config::Config::Params(p)) => {
                                Some(p.size as usize)
                            }
                            _ => None,
                        })
                        .unwrap_or(0);
                    result.push(CollectionInfo {
                        name: col.name,
                        document_count: count,
                        dimensions: dims,
                    });
                }
            }
        }

        Ok(result)
    }

    async fn collection_exists(&self, name: &str) -> Result<bool> {
        let collections = self
            .client
            .list_collections()
            .await
            .map_err(|e| AppError::Database(format!("Failed to list collections: {}", e)))?;

        Ok(collections.collections.iter().any(|c| c.name == name))
    }

    async fn collection_stats(&self, name: &str) -> Result<CollectionStats> {
        let info = self
            .client
            .collection_info(name)
            .await
            .map_err(|e| AppError::Database(format!("Failed to get collection info: {}", e)))?;

        let result = info
            .result
            .ok_or_else(|| AppError::Database("Collection not found".to_string()))?;

        let document_count = result.points_count.unwrap_or(0) as usize;
        let dimensions = result
            .config
            .and_then(|c| c.params)
            .and_then(|p| p.vectors_config)
            .and_then(|v| match v.config {
                Some(qdrant_client::qdrant::vectors_config::Config::Params(p)) => {
                    Some(p.size as usize)
                }
                _ => None,
            })
            .unwrap_or(0);

        Ok(CollectionStats {
            name: name.to_string(),
            document_count,
            dimensions,
            index_size_bytes: None,
            distance_metric: "cosine".to_string(),
        })
    }

    async fn upsert(&self, collection: &str, documents: &[Document]) -> Result<usize> {
        let mut points = Vec::with_capacity(documents.len());

        for document in documents {
            let embedding = document
                .embedding
                .as_ref()
                .ok_or_else(|| AppError::Database("Document missing embedding".to_string()))?;

            let mut payload: HashMap<String, qdrant_client::qdrant::Value> = HashMap::new();
            payload.insert("content".to_string(), document.content.clone().into());
            payload.insert("title".to_string(), document.metadata.title.clone().into());
            payload.insert(
                "source".to_string(),
                document.metadata.source.clone().into(),
            );
            payload.insert(
                "created_at".to_string(),
                document.metadata.created_at.timestamp().into(),
            );
            payload.insert(
                "tags".to_string(),
                serde_json::to_value(&document.metadata.tags)
                    .unwrap_or(serde_json::Value::Null)
                    .into(),
            );

            points.push(PointStruct::new(
                document.id.clone(),
                embedding.clone(),
                payload,
            ));
        }

        let count = points.len();
        self.client
            .upsert_points(UpsertPointsBuilder::new(collection, points).wait(true))
            .await
            .map_err(|e| AppError::Database(format!("Failed to upsert points: {}", e)))?;

        Ok(count)
    }

    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>> {
        let search_builder = SearchPointsBuilder::new(collection, embedding.to_vec(), limit as u64)
            .score_threshold(threshold);

        let search_result = self
            .client
            .search_points(search_builder.with_payload(true))
            .await
            .map_err(|e| AppError::Database(format!("Failed to search: {}", e)))?;

        Ok(self.parse_search_results(search_result))
    }

    async fn search_with_filters(
        &self,
        collection: &str,
        embedding: &[f32],
        limit: usize,
        threshold: f32,
        filters: &[(String, String)],
    ) -> Result<Vec<SearchResult>> {
        let mut search_builder =
            SearchPointsBuilder::new(collection, embedding.to_vec(), limit as u64)
                .score_threshold(threshold);

        if !filters.is_empty() {
            let conditions: Vec<Condition> = filters
                .iter()
                .map(|(field, value)| {
                    let field_condition = FieldCondition {
                        key: field.clone(),
                        r#match: Some(Match {
                            match_value: Some(MatchValue::Text(value.clone())),
                        }),
                        ..Default::default()
                    };
                    Condition {
                        condition_one_of: Some(ConditionOneOf::Field(field_condition)),
                    }
                })
                .collect();
            search_builder = search_builder.filter(Filter::must(conditions));
        }

        let search_result = self
            .client
            .search_points(search_builder.with_payload(true))
            .await
            .map_err(|e| AppError::Database(format!("Failed to search: {}", e)))?;

        Ok(self.parse_search_results(search_result))
    }

    async fn delete(&self, collection: &str, ids: &[String]) -> Result<usize> {
        use qdrant_client::qdrant::point_id::PointIdOptions;

        let point_ids: Vec<PointId> = ids
            .iter()
            .map(|id| {
                if let Ok(num) = id.parse::<u64>() {
                    PointId {
                        point_id_options: Some(PointIdOptions::Num(num)),
                    }
                } else {
                    PointId {
                        point_id_options: Some(PointIdOptions::Uuid(id.to_string())),
                    }
                }
            })
            .collect();

        let count = point_ids.len();
        self.client
            .delete_points(
                DeletePointsBuilder::new(collection)
                    .points(point_ids)
                    .wait(true),
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete points: {}", e)))?;

        Ok(count)
    }

    async fn get(&self, collection: &str, id: &str) -> Result<Option<Document>> {
        use qdrant_client::qdrant::{point_id::PointIdOptions, GetPointsBuilder, PointId};

        // Try to parse the ID as a numeric ID first, otherwise use UUID
        let point_id = if let Ok(num) = id.parse::<u64>() {
            PointId {
                point_id_options: Some(PointIdOptions::Num(num)),
            }
        } else {
            PointId {
                point_id_options: Some(PointIdOptions::Uuid(id.to_string())),
            }
        };

        let result = self
            .client
            .get_points(
                GetPointsBuilder::new(collection, vec![point_id])
                    .with_payload(true)
                    .with_vectors(true),
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to get point: {}", e)))?;

        // Extract the first point if found
        let point = match result.result.into_iter().next() {
            Some(p) => p,
            None => return Ok(None),
        };

        // Parse the payload
        let payload = point.payload;
        let content = payload
            .get("content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let title = payload
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let source = payload
            .get("source")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let created_at_ts = payload
            .get("created_at")
            .and_then(|v| v.as_integer())
            .unwrap_or(0);
        let tags: Vec<String> = payload
            .get("tags")
            .and_then(|v| serde_json::from_value(v.clone().into()).ok())
            .unwrap_or_default();

        // Get the ID string
        let id_str = match point.id {
            Some(pid) => match pid.point_id_options {
                Some(PointIdOptions::Num(num)) => num.to_string(),
                Some(PointIdOptions::Uuid(uuid)) => uuid,
                None => return Ok(None),
            },
            None => return Ok(None),
        };

        // Extract embedding if available
        // Note: For simplicity, we don't return the embedding when getting by ID.
        // If embeddings are needed, use the search methods instead.
        let embedding = None;

        Ok(Some(Document {
            id: id_str,
            content,
            metadata: crate::types::DocumentMetadata {
                title,
                source,
                created_at: chrono::DateTime::from_timestamp(created_at_ts, 0)
                    .unwrap_or_else(chrono::Utc::now),
                tags,
            },
            embedding,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::{AppError, DocumentMetadata};
    use chrono::Utc;

    fn sample_document(id: &str) -> Document {
        Document {
            id: id.to_string(),
            content: format!("content for {id}"),
            metadata: DocumentMetadata {
                title: format!("Title {id}"),
                source: "test".to_string(),
                created_at: Utc::now(),
                tags: vec!["alpha".to_string(), "beta".to_string()],
            },
            embedding: Some(vec![0.1, 0.2, 0.3]),
        }
    }

    // ── Connection logic ─────────────────────────────────────────────────

    #[test]
    fn default_qdrant_url_matches_constant() {
        assert_eq!(default_qdrant_url(), DEFAULT_QDRANT_URL);
    }

    #[test]
    fn resolve_qdrant_url_prefers_explicit_override() {
        std::env::remove_var("QDRANT_URL");
        assert_eq!(
            resolve_qdrant_url(Some("http://override:6334")),
            "http://override:6334"
        );
    }

    #[test]
    fn resolve_qdrant_url_trims_override() {
        std::env::remove_var("QDRANT_URL");
        assert_eq!(
            resolve_qdrant_url(Some("  http://trimmed:6334  ")),
            "http://trimmed:6334"
        );
    }

    #[test]
    fn resolve_qdrant_url_falls_back_to_default_when_env_missing() {
        std::env::remove_var("QDRANT_URL");
        assert_eq!(resolve_qdrant_url(None), default_qdrant_url());
    }

    #[test]
    fn resolve_qdrant_url_ignores_blank_override() {
        std::env::remove_var("QDRANT_URL");
        assert_eq!(resolve_qdrant_url(Some("   ")), default_qdrant_url());
    }

    #[test]
    fn resolve_qdrant_api_key_prefers_override() {
        std::env::remove_var("QDRANT_API_KEY");
        assert_eq!(
            resolve_qdrant_api_key(Some("override-key")),
            Some("override-key".to_string())
        );
    }

    #[test]
    fn resolve_qdrant_api_key_ignores_blank_override() {
        std::env::remove_var("QDRANT_API_KEY");
        assert_eq!(resolve_qdrant_api_key(Some("   ")), None);
    }

    // ── Query building ─────────────────────────────────────────────────

    #[test]
    fn validate_qdrant_url_accepts_http_and_https() {
        assert!(validate_qdrant_url("http://localhost:6334").is_ok());
        assert!(validate_qdrant_url("https://qdrant.example:6334").is_ok());
    }

    #[test]
    fn validate_qdrant_url_rejects_invalid_urls() {
        assert!(validate_qdrant_url("").is_err());
        assert!(validate_qdrant_url("grpc://localhost:6334").is_err());
    }

    #[test]
    fn validate_collection_name_rejects_invalid() {
        assert!(validate_collection_name("").is_err());
        assert!(validate_collection_name("1bad").is_err());
        assert!(validate_collection_name("has-dash").is_err());
    }

    #[test]
    fn validate_search_params_rejects_out_of_range_threshold() {
        assert!(validate_search_params(10, 1.5).is_err());
        assert!(validate_search_params(0, 0.5).is_err());
    }

    #[test]
    fn classify_point_id_parses_numeric_and_uuid() {
        assert_eq!(classify_point_id("42"), PointIdKind::Numeric(42));
        assert_eq!(
            classify_point_id("doc-uuid"),
            PointIdKind::Uuid("doc-uuid".to_string())
        );
    }

    #[test]
    fn build_document_payload_fields_includes_metadata() {
        let doc = sample_document("doc1");
        let payload = build_document_payload_fields(&doc);
        assert_eq!(payload.get("content").and_then(|v| v.as_str()), Some("content for doc1"));
        assert_eq!(payload.get("title").and_then(|v| v.as_str()), Some("Title doc1"));
        assert_eq!(payload.get("tags").and_then(|v| v.as_array()).map(|a| a.len()), Some(2));
    }

    #[test]
    fn build_filter_field_keys_preserves_order() {
        let filters = vec![
            ("source".to_string(), "web".to_string()),
            ("title".to_string(), "Intro".to_string()),
        ];
        assert_eq!(
            build_filter_field_keys(&filters),
            vec!["source".to_string(), "title".to_string()]
        );
    }

    #[test]
    fn describe_search_request_formats_parameters() {
        let desc = describe_search_request("documents", 5, 0.75, &[]).expect("describe");
        assert!(desc.contains("collection=documents"));
        assert!(desc.contains("limit=5"));
        assert!(desc.contains("threshold=0.75"));
    }

    #[test]
    fn describe_search_request_rejects_invalid_limit() {
        assert!(describe_search_request("documents", 0, 0.5, &[]).is_err());
    }

    // ── Serde: QdrantConfig ──────────────────────────────────────────────

    #[test]
    fn qdrant_config_default_values() {
        let config = QdrantConfig::default();
        assert_eq!(config.url, default_qdrant_url());
        assert!(config.api_key.is_none());
        assert_eq!(config.default_dimensions, DEFAULT_VECTOR_DIMENSIONS);
    }

    #[test]
    fn qdrant_config_serde_roundtrip() {
        let config = QdrantConfig {
            url: "http://remote:6334".into(),
            api_key: Some("secret".into()),
            default_dimensions: 1536,
        };
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: QdrantConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, config);
    }

    #[test]
    fn qdrant_config_deserializes_with_defaults() {
        let json = r#"{"url":"http://custom:6334"}"#;
        let config: QdrantConfig = serde_json::from_str(json).expect("deserialize");
        assert_eq!(config.url, "http://custom:6334");
        assert!(config.api_key.is_none());
        assert_eq!(config.default_dimensions, DEFAULT_VECTOR_DIMENSIONS);
    }

    // ── Error handling (live client requires qdrant feature) ─────────────

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn new_rejects_invalid_url() {
        let err = QdrantVectorStore::new("not-a-url".into(), None)
            .await
            .unwrap_err();
        matches::assert_matches!(err, AppError::Database(_));
    }
}
