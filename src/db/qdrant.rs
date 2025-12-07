use crate::types::{AppError, Document, Result, SearchQuery, SearchResult};
use qdrant_client::{
    qdrant::{
        Condition, CreateCollectionBuilder, DeletePointsBuilder, Distance, FieldCondition, Filter,
        Match, PointStruct, SearchPointsBuilder, UpsertPointsBuilder, VectorParamsBuilder,
    },
    Qdrant,
};
use std::collections::HashMap;

pub struct QdrantClient {
    client: Qdrant,
}

impl QdrantClient {
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

        let mut payload = HashMap::new();
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

    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        let collection_name = "documents";

        // Convert query to embedding (this should be done by the caller)
        // For now, we assume the query string is already embedded
        // In practice, you'd call an embedding service here

        let mut search_builder = SearchPointsBuilder::new(
            collection_name,
            vec![], // Placeholder - needs actual query embedding
            query.limit as u64,
        )
        .score_threshold(query.threshold);

        // Add filters if provided
        if let Some(filters) = &query.filters {
            let mut conditions = Vec::new();
            for filter in filters {
                conditions.push(Condition::from(FieldCondition::new_match(
                    filter.field.clone(),
                    Match::from(filter.value.clone()),
                )));
            }
            search_builder = search_builder.filter(Filter::must(conditions));
        }

        let search_result = self
            .client
            .search_points(search_builder.with_payload(true))
            .await
            .map_err(|e| AppError::Database(format!("Failed to search: {}", e)))?;

        let results = search_result
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

                Some(SearchResult {
                    document: Document {
                        id: scored_point.id.as_ref()?.to_string(),
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
            .collect();

        Ok(results)
    }

    pub async fn delete_document(&self, id: &str) -> Result<()> {
        let collection_name = "documents";

        // collection_name, &[id.into()], None

        self.client
            .delete_points(
                DeletePointsBuilder::new(collection_name)
                    .points(vec![id.to_string().into()])
                    .wait(true),
            )
            .await
            .map_err(|e| AppError::Database(format!("Failed to delete point: {}", e)))?;

        Ok(())
    }
}
