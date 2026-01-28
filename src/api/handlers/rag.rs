//! RAG (Retrieval Augmented Generation) API handlers.
//!
//! Provides endpoints for:
//! - Document ingestion with chunking
//! - Multi-strategy search (semantic, BM25, fuzzy, hybrid)
//! - Collection management

use crate::{
    auth::middleware::AuthUser,
    db::{AresVectorStore, VectorStore},
    rag::{
        chunker::{ChunkingStrategy, TextChunker},
        embeddings::{EmbeddingModelType, EmbeddingService},
        reranker::{Reranker, RerankerConfig, RerankerModelType},
        search::{HybridWeights, SearchEngine, SearchStrategy},
    },
    types::{
        AppError, Document, DocumentMetadata, RagDeleteCollectionRequest,
        RagDeleteCollectionResponse, RagIngestRequest, RagIngestResponse, RagSearchRequest,
        RagSearchResponse, RagSearchResult, Result,
    },
    AppState,
};
use axum::{extract::State, Json};
use chrono::Utc;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::OnceCell;
use uuid::Uuid;

// ============================================================================
// User Isolation
// ============================================================================

/// Prefix collection name with user ID for isolation.
/// All RAG collections are scoped per-user to prevent data leakage.
fn user_scoped_collection(user_id: &str, collection: &str) -> String {
    format!("user_{}_{}", user_id, collection)
}

/// Extract user-friendly collection name from scoped name.
/// Returns None if the collection doesn't belong to the user.
fn extract_user_collection(user_id: &str, scoped_name: &str) -> Option<String> {
    let prefix = format!("user_{}_", user_id);
    scoped_name.strip_prefix(&prefix).map(|s| s.to_string())
}

// ============================================================================
// Shared RAG Services
// ============================================================================

/// Global embedding service (lazy initialized).
static EMBEDDING_SERVICE: OnceCell<Arc<EmbeddingService>> = OnceCell::const_new();

/// Get or create the embedding service.
async fn get_embedding_service() -> Result<Arc<EmbeddingService>> {
    EMBEDDING_SERVICE
        .get_or_try_init(|| async {
            let service = EmbeddingService::with_model(EmbeddingModelType::default())
                .map_err(|e| AppError::Internal(format!("Failed to init embeddings: {}", e)))?;
            Ok::<_, AppError>(Arc::new(service))
        })
        .await
        .cloned()
}

/// Global vector store (lazy initialized).
/// NOTE: Uses default path "./data/vectors". For config-driven path, consider moving
/// vector store initialization to AppState setup in lib.rs, similar to how
/// config_manager and provider_registry are initialized at startup.
static VECTOR_STORE: OnceCell<Arc<AresVectorStore>> = OnceCell::const_new();

/// Get or create the vector store.
/// Uses the default path. For proper config integration, the VectorStore should
/// be initialized in AppState using config_manager.config().rag.vector_path.
async fn get_vector_store() -> Result<Arc<AresVectorStore>> {
    VECTOR_STORE
        .get_or_try_init(|| async {
            // Default path matches ares.example.toml [rag] vector_path default
            let store = AresVectorStore::new(Some("./data/vectors".to_string())).await?;
            Ok::<_, AppError>(Arc::new(store))
        })
        .await
        .cloned()
}

// ============================================================================
// Ingest Endpoint
// ============================================================================

/// Ingest a document into the RAG system.
///
/// Chunks the document and stores embeddings for later retrieval.
#[utoipa::path(
    post,
    path = "/api/rag/ingest",
    request_body = RagIngestRequest,
    responses(
        (status = 200, description = "Document ingested successfully", body = RagIngestResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "rag",
    security(("bearer" = []))
)]
pub async fn ingest(
    State(_state): State<AppState>,
    AuthUser(claims): AuthUser,
    Json(payload): Json<RagIngestRequest>,
) -> Result<Json<RagIngestResponse>> {
    let start = Instant::now();

    // Validate input
    if payload.collection.is_empty() {
        return Err(AppError::InvalidInput("Collection name required".into()));
    }
    if payload.content.is_empty() {
        return Err(AppError::InvalidInput("Content required".into()));
    }

    // Scope collection to user for isolation
    let scoped_collection = user_scoped_collection(&claims.sub, &payload.collection);

    // Get services
    let embedding_service = get_embedding_service().await?;
    let vector_store = get_vector_store().await?;

    // Parse chunking strategy
    let strategy: ChunkingStrategy = payload
        .chunking_strategy
        .as_ref()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or_default();

    // Create chunker
    let chunker = match strategy {
        ChunkingStrategy::Word => TextChunker::with_word_chunking(200, 50),
        ChunkingStrategy::Semantic => TextChunker::with_semantic_chunking(500),
        ChunkingStrategy::Character => TextChunker::with_character_chunking(500, 100),
    };

    // Chunk the content
    let chunks = chunker.chunk_with_metadata(&payload.content);

    if chunks.is_empty() {
        return Err(AppError::InvalidInput("Content too small to chunk".into()));
    }

    // Ensure collection exists
    let dimensions = embedding_service.dimensions();
    if !vector_store.collection_exists(&scoped_collection).await? {
        vector_store
            .create_collection(&scoped_collection, dimensions)
            .await?;
    }

    // Generate embeddings for each chunk
    let chunk_texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
    let embeddings = embedding_service.embed_texts(&chunk_texts).await?;

    // Create documents
    let base_id = Uuid::new_v4().to_string();
    let mut documents = Vec::with_capacity(chunks.len());
    let mut document_ids = Vec::with_capacity(chunks.len());

    for (i, (chunk, embedding)) in chunks.iter().zip(embeddings.into_iter()).enumerate() {
        let doc_id = format!("{}_{}", base_id, i);
        document_ids.push(doc_id.clone());

        documents.push(Document {
            id: doc_id,
            content: chunk.content.clone(),
            metadata: DocumentMetadata {
                title: payload.title.clone().unwrap_or_default(),
                source: payload.source.clone().unwrap_or_default(),
                created_at: Utc::now(),
                tags: payload.tags.clone(),
            },
            embedding: Some(embedding),
        });
    }

    // Upsert to vector store
    let count = vector_store.upsert(&scoped_collection, &documents).await?;

    tracing::info!(
        user_id = %claims.sub,
        collection = %payload.collection,
        scoped_collection = %scoped_collection,
        chunks = count,
        duration_ms = start.elapsed().as_millis() as u64,
        "Document ingested"
    );

    Ok(Json(RagIngestResponse {
        chunks_created: count,
        document_ids,
        collection: payload.collection, // Return user-facing name, not scoped
    }))
}

// ============================================================================
// Search Endpoint
// ============================================================================

/// Search the RAG system.
///
/// Supports multiple search strategies: semantic, BM25, fuzzy, and hybrid.
#[utoipa::path(
    post,
    path = "/api/rag/search",
    request_body = RagSearchRequest,
    responses(
        (status = 200, description = "Search completed", body = RagSearchResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Collection not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "rag",
    security(("bearer" = []))
)]
pub async fn search(
    State(_state): State<AppState>,
    AuthUser(claims): AuthUser,
    Json(payload): Json<RagSearchRequest>,
) -> Result<Json<RagSearchResponse>> {
    let start = Instant::now();

    // Validate input
    if payload.collection.is_empty() {
        return Err(AppError::InvalidInput("Collection name required".into()));
    }
    if payload.query.is_empty() {
        return Err(AppError::InvalidInput("Query required".into()));
    }

    // Scope collection to user for isolation
    let scoped_collection = user_scoped_collection(&claims.sub, &payload.collection);

    // Get services
    let embedding_service = get_embedding_service().await?;
    let vector_store = get_vector_store().await?;

    // Check collection exists
    if !vector_store.collection_exists(&scoped_collection).await? {
        return Err(AppError::NotFound(format!(
            "Collection '{}' not found",
            payload.collection
        )));
    }

    // Parse search strategy
    let strategy: SearchStrategy = payload
        .strategy
        .as_ref()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(SearchStrategy::Semantic);

    // Generate query embedding
    let query_embedding = embedding_service.embed_text(&payload.query).await?;

    // Perform vector search
    let vector_results = vector_store
        .search(
            &scoped_collection,
            &query_embedding,
            payload.limit * 2, // Fetch extra for filtering/reranking
            payload.threshold,
        )
        .await?;

    // Apply additional search strategies if needed
    let mut results: Vec<RagSearchResult> = match strategy {
        SearchStrategy::Semantic => {
            // Pure semantic search - already done
            vector_results
                .iter()
                .take(payload.limit)
                .map(|r| RagSearchResult {
                    id: r.document.id.clone(),
                    content: r.document.content.clone(),
                    score: r.score,
                    metadata: r.document.metadata.clone(),
                })
                .collect()
        }
        SearchStrategy::Bm25 | SearchStrategy::Fuzzy | SearchStrategy::Hybrid => {
            // For BM25, fuzzy, or hybrid, we need to build an index over the results
            let mut search_engine = SearchEngine::new();

            // Index the vector search results as Document structs
            for r in &vector_results {
                search_engine.index_document(&r.document);
            }

            // Get strategy-specific results
            let strategy_results = match strategy {
                SearchStrategy::Bm25 => search_engine.search_bm25(&payload.query, payload.limit),
                SearchStrategy::Fuzzy => search_engine.search_fuzzy(&payload.query, payload.limit),
                SearchStrategy::Hybrid => {
                    // Combine semantic and BM25 using hybrid search
                    let semantic_scores: Vec<_> = vector_results
                        .iter()
                        .map(|r| (r.document.id.clone(), r.score))
                        .collect();
                    let weights = HybridWeights::default();
                    search_engine.search_hybrid(
                        &payload.query,
                        &semantic_scores,
                        &weights,
                        payload.limit,
                    )
                }
                _ => vec![], // Already handled above
            };

            // Map back to full documents
            strategy_results
                .iter()
                .filter_map(|(id, score)| {
                    vector_results
                        .iter()
                        .find(|r| r.document.id == *id)
                        .map(|r| RagSearchResult {
                            id: r.document.id.clone(),
                            content: r.document.content.clone(),
                            score: *score,
                            metadata: r.document.metadata.clone(),
                        })
                })
                .collect()
        }
    };

    // Apply reranking if requested
    let reranked = if payload.rerank && !results.is_empty() {
        // Parse reranker model
        let model_type: RerankerModelType = payload
            .reranker_model
            .as_ref()
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or_default();

        // Create reranker with config
        let config = RerankerConfig {
            model: model_type,
            ..Default::default()
        };
        let reranker = Reranker::new(config);

        // Prepare results for reranking: (id, content, score)
        let rerank_input: Vec<_> = results
            .iter()
            .map(|r| (r.id.clone(), r.content.clone(), r.score))
            .collect();

        // Rerank results
        let reranked_results = reranker
            .rerank(&payload.query, &rerank_input, Some(payload.limit))
            .await
            .map_err(|e| AppError::Internal(format!("Reranking failed: {}", e)))?;

        // Convert to RagSearchResult
        results = reranked_results
            .into_iter()
            .filter_map(|rr| {
                results
                    .iter()
                    .find(|r| r.id == rr.id)
                    .map(|r| RagSearchResult {
                        id: r.id.clone(),
                        content: r.content.clone(),
                        score: rr.final_score,
                        metadata: r.metadata.clone(),
                    })
            })
            .collect();
        true
    } else {
        false
    };

    let total = results.len();
    let strategy_name = format!("{:?}", strategy).to_lowercase();

    tracing::info!(
        user_id = %claims.sub,
        collection = %payload.collection,
        strategy = %strategy_name,
        results = total,
        reranked = reranked,
        duration_ms = start.elapsed().as_millis() as u64,
        "Search completed"
    );

    Ok(Json(RagSearchResponse {
        results,
        total,
        strategy: strategy_name,
        reranked,
        duration_ms: start.elapsed().as_millis() as u64,
    }))
}

// ============================================================================
// Delete Collection Endpoint
// ============================================================================

/// Delete a RAG collection.
#[utoipa::path(
    delete,
    path = "/api/rag/collection",
    request_body = RagDeleteCollectionRequest,
    responses(
        (status = 200, description = "Collection deleted", body = RagDeleteCollectionResponse),
        (status = 400, description = "Invalid request"),
        (status = 401, description = "Unauthorized"),
        (status = 404, description = "Collection not found"),
        (status = 500, description = "Internal server error")
    ),
    tag = "rag",
    security(("bearer" = []))
)]
pub async fn delete_collection(
    State(_state): State<AppState>,
    AuthUser(claims): AuthUser,
    Json(payload): Json<RagDeleteCollectionRequest>,
) -> Result<Json<RagDeleteCollectionResponse>> {
    // Validate input
    if payload.collection.is_empty() {
        return Err(AppError::InvalidInput("Collection name required".into()));
    }

    // Scope collection to user for isolation
    let scoped_collection = user_scoped_collection(&claims.sub, &payload.collection);

    let vector_store = get_vector_store().await?;

    // Check collection exists
    if !vector_store.collection_exists(&scoped_collection).await? {
        return Err(AppError::NotFound(format!(
            "Collection '{}' not found",
            payload.collection
        )));
    }

    // Get document count before deletion
    let stats = vector_store.collection_stats(&scoped_collection).await?;
    let doc_count = stats.document_count;

    // Delete the collection
    vector_store.delete_collection(&scoped_collection).await?;

    tracing::info!(
        user_id = %claims.sub,
        collection = %payload.collection,
        documents = doc_count,
        "Collection deleted"
    );

    Ok(Json(RagDeleteCollectionResponse {
        success: true,
        collection: payload.collection, // Return user-facing name
        documents_deleted: doc_count,
    }))
}

// ============================================================================
// List Collections Endpoint
// ============================================================================

/// List all RAG collections.
#[utoipa::path(
    get,
    path = "/api/rag/collections",
    responses(
        (status = 200, description = "Collections listed", body = Vec<String>),
        (status = 401, description = "Unauthorized"),
        (status = 500, description = "Internal server error")
    ),
    tag = "rag",
    security(("bearer" = []))
)]
pub async fn list_collections(
    State(_state): State<AppState>,
    AuthUser(claims): AuthUser,
) -> Result<Json<Vec<crate::db::CollectionInfo>>> {
    let vector_store = get_vector_store().await?;
    let all_collections = vector_store.list_collections().await?;

    // Filter to only collections belonging to this user and unscope names
    let user_collections: Vec<_> = all_collections
        .into_iter()
        .filter_map(|mut info| {
            extract_user_collection(&claims.sub, &info.name).map(|user_name| {
                info.name = user_name;
                info
            })
        })
        .collect();

    Ok(Json(user_collections))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_search_strategy() {
        let strategy: SearchStrategy = "semantic".parse().unwrap();
        assert_eq!(strategy, SearchStrategy::Semantic);

        let strategy: SearchStrategy = "bm25".parse().unwrap();
        assert_eq!(strategy, SearchStrategy::Bm25);

        let strategy: SearchStrategy = "hybrid".parse().unwrap();
        assert_eq!(strategy, SearchStrategy::Hybrid);
    }

    #[test]
    fn test_default_chunking_strategy() {
        let strategy: ChunkingStrategy = "word".parse().unwrap();
        assert_eq!(strategy, ChunkingStrategy::Word);

        let strategy: ChunkingStrategy = "semantic".parse().unwrap();
        assert_eq!(strategy, ChunkingStrategy::Semantic);
    }
}
