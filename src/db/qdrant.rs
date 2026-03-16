use crate::types::{AppError, Document, Result, SearchResult};
use async_trait::async_trait;
use qdrant_client::{
    qdrant::{
        condition::ConditionOneOf, r#match::MatchValue, Condition, CreateCollectionBuilder,
        DeletePointsBuilder, Distance, FieldCondition, Filter, Match, PointId, PointStruct,
        SearchPointsBuilder, UpsertPointsBuilder, VectorParamsBuilder,
    },
    Qdrant,
};
use std::collections::HashMap;

use super::vectorstore::{CollectionInfo, CollectionStats, VectorStore};

/// Qdrant vector store implementation.
///
/// Provides vector storage and similarity search using a Qdrant server.
/// Requires a running Qdrant instance.
pub struct QdrantVectorStore {
    client: Qdrant,
}

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
