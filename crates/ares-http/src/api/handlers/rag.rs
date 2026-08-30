//! RAG (Retrieval Augmented Generation) API handlers.
//!
//! Provides endpoints for:
//! - Document ingestion with chunking
//! - Multi-strategy search (semantic, BM25, fuzzy, hybrid)
//! - Collection management

use crate::{
    auth::middleware::AuthUser,
    db::{tenant_allowlist as allowlist, AresVectorStore, VectorStore},
    rag::{
        chunker::{ChunkingStrategy, TextChunker},
        search::{HybridWeights, SearchEngine, SearchStrategy},
    },
    types::{
        AppError, Document, DocumentMetadata, RagDeleteCollectionRequest,
        RagDeleteCollectionResponse, RagIngestRequest, RagIngestResponse, RagSearchRequest,
        RagSearchResponse, RagSearchResult,
    },
    HttpError, Result,
};
#[cfg(feature = "local-embeddings")]
use crate::rag::{
    embeddings::{EmbeddingModelType, EmbeddingService},
    reranker::{Reranker, RerankerConfig, RerankerModelType},
};
use axum::{extract::State, Json};
use chrono::Utc;
use cordis::Context;
use std::sync::Arc;
use std::time::Instant;
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

fn filter_collections_by_allowed_sources(
    collections: Vec<ares_store::CollectionInfo>,
    allowed_sources: &[allowlist::TenantRagAllowlistItem],
) -> Vec<ares_store::CollectionInfo> {
    if allowed_sources.is_empty() {
        return Vec::new();
    }

    let allowed: std::collections::HashSet<&str> = allowed_sources
        .iter()
        .filter(|source| source.enabled)
        .map(|source| source.rag_source.as_str())
        .collect();

    collections
        .into_iter()
        .filter(|info| allowed.contains(info.name.as_str()))
        .collect()
}

// ============================================================================
// Shared RAG Services
// ============================================================================

/// Construct EmbeddingService without a process-global cache.
///
/// Pre-downloads model files via lancor (reqwest) before fastembed init,
/// because fastembed's hf-hub/ureq client fails on HuggingFace's xethub CDN.
/// Context is the singleton: callers must `provide_arc` after construction.
#[cfg(feature = "local-embeddings")]
async fn construct_embedding_service() -> ares_types::types::Result<Arc<EmbeddingService>> {
    // Pre-download ONNX model via lancor before fastembed tries with ureq
    let model = EmbeddingModelType::default();
    let cache_dir =
        std::env::var("FASTEMBED_CACHE_DIR").unwrap_or_else(|_| ".fastembed_cache".to_string());
    let cache_path = std::path::PathBuf::from(&cache_dir);

    match lancor::hub::HubClient::with_cache_dir(cache_path.clone()) {
        Err(e) => tracing::error!("Failed to create lancor HubClient: {}", e),
        Ok(hub) => {
            let repo_id = model.hf_repo_id();
            for filename in &[
                "onnx/model.onnx",
                "tokenizer.json",
                "config.json",
                "tokenizer_config.json",
            ] {
                // Build HF cache path
                let folder = format!("models--{}", repo_id.replace('/', "--"));
                let snapshot_dir = cache_path.join(&folder).join("snapshots").join("lancor");
                let target = snapshot_dir.join(filename);

                if target.exists()
                    && std::fs::metadata(&target)
                        .map(|m| m.len() > 0)
                        .unwrap_or(false)
                {
                    tracing::debug!("Model file cached: {}", target.display());
                    continue;
                }

                tracing::info!("Downloading {}/{} via lancor...", repo_id, filename);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).ok();
                }

                match hub.download(repo_id, filename, None).await {
                    Ok(dl_path) => {
                        if dl_path != target {
                            std::fs::copy(&dl_path, &target).ok();
                        }
                        tracing::info!(
                            "Downloaded: {} ({} bytes)",
                            filename,
                            std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0)
                        );
                    }
                    Err(e) => tracing::warn!("Could not download {}: {}", filename, e),
                }
            }

            // Write refs/main
            let refs_dir = cache_path
                .join(format!("models--{}", repo_id.replace('/', "--")))
                .join("refs");
            std::fs::create_dir_all(&refs_dir).ok();
            std::fs::write(refs_dir.join("main"), "lancor").ok();
        }
    }

    let service = EmbeddingService::with_model(model)
        .map_err(|e| AppError::Internal(format!("Failed to init embeddings: {}", e)))?;
    Ok(Arc::new(service))
}

/// Resolve EmbeddingService from Context, constructing on miss.
/// Construction (model download) happens on first RAG request, not at boot.
/// Context is the singleton; there is no process-global cache.
#[cfg(feature = "local-embeddings")]
pub async fn embedding_service_from_ctx(
    ctx: &std::sync::Arc<cordis::Context>,
) -> ares_types::types::Result<Arc<EmbeddingService>> {
    if let Some(existing) = ctx.get::<EmbeddingService>() {
        return Ok(existing);
    }
    let created = construct_embedding_service().await?;
    ctx.provide_arc(created.clone());
    Ok(created)
}

/// Resolve AresVectorStore from Context, constructing on miss.
/// Context is the singleton; there is no process-global cache.
pub async fn vector_store_from_ctx(
    ctx: &std::sync::Arc<cordis::Context>,
    vector_path: &str,
) -> ares_types::types::Result<Arc<AresVectorStore>> {
    if let Some(existing) = ctx.get::<AresVectorStore>() {
        return Ok(existing);
    }
    let store = Arc::new(AresVectorStore::new(Some(vector_path.to_string())).await?);
    ctx.provide_arc(store.clone());
    Ok(store)
}

/// Embed texts for RAG ingest and search.
///
/// Local path uses [`EmbeddingService`]. Remote path uses [`ares_rag::embed_with_llm`].
async fn embed_for_rag(
    ctx: &Arc<Context>,
    texts: &[String],
) -> ares_types::types::Result<Vec<Vec<f32>>> {
    #[cfg(feature = "local-embeddings")]
    {
        let embedding_service = embedding_service_from_ctx(ctx).await?;
        embedding_service.embed_texts(texts).await
    }
    #[cfg(not(feature = "local-embeddings"))]
    {
        ares_rag::embed_with_llm(ctx, texts).await
    }
}

// ============================================================================
// Ingest Endpoint
// ============================================================================

/// Ingest a document into the RAG system.
///
/// Chunks the document and stores embeddings for later retrieval.
pub async fn ingest(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Json(payload): Json<RagIngestRequest>,
) -> Result<Json<RagIngestResponse>> {
    let start = Instant::now();

    // Validate input
    if payload.collection.is_empty() {
        return Err(HttpError::from(AppError::InvalidInput(
            "Collection name required".to_string(),
        )));
    }
    if payload.content.is_empty() {
        return Err(HttpError::from(AppError::InvalidInput(
            "Content required".to_string(),
        )));
    }

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let allowlist_store = allowlist::TenantAllowlistStore::new(&pool);
    if !allowlist_store
        .is_rag_source_allowed(&claims.sub, &payload.collection)
        .await?
    {
        return Err(HttpError::from(AppError::Auth(
            format!(
                "RAG source '{}' is not allowed for this tenant",
                payload.collection
            )
            .into(),
        )));
    }

    // Scope collection to user for isolation
    let scoped_collection = user_scoped_collection(&claims.sub, &payload.collection);

    // Get services
    let config = ctx
        .get::<crate::overlay::AresConfigManager>()
        .expect("not provided")
        .config();
    let vector_path = &config.rag.vector.vector_path;
    let vector_store = vector_store_from_ctx(&ctx, vector_path).await?;

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
        return Err(HttpError::from(AppError::InvalidInput(
            "Content too small to chunk".to_string(),
        )));
    }

    // Generate embeddings for each chunk, then size the collection from the first vector.
    let chunk_texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
    let embeddings = embed_for_rag(&ctx, &chunk_texts).await?;
    let dimensions = embeddings
        .first()
        .map(|v| v.len())
        .ok_or_else(|| AppError::Internal("No embedding generated".to_string()))?;
    if !vector_store.collection_exists(&scoped_collection).await? {
        vector_store
            .create_collection(&scoped_collection, dimensions)
            .await?;
    }

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
pub async fn search(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Json(payload): Json<RagSearchRequest>,
) -> Result<Json<RagSearchResponse>> {
    let start = Instant::now();
    // Respect RAG feature flag
    if !ctx
        .get::<crate::overlay::AresConfigManager>()
        .expect("not provided")
        .config()
        .rag
        .vector
        .enabled
    {
        return Err(HttpError::from(AppError::FeatureDisabled(
            "RAG feature is disabled. Set `[rag.vector] enabled = true` in ares.toml".into(),
        )));
    }

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let allowlist_store = allowlist::TenantAllowlistStore::new(&pool);
    if !allowlist_store
        .is_rag_source_allowed(&claims.sub, &payload.collection)
        .await?
    {
        return Err(HttpError::from(AppError::Auth(
            format!(
                "RAG source '{}' is not allowed for this tenant",
                payload.collection
            )
            .into(),
        )));
    }

    // Validate input
    // Scope collection to user for isolation
    let scoped_collection = user_scoped_collection(&claims.sub, &payload.collection);

    // Get services
    let config = ctx
        .get::<crate::overlay::AresConfigManager>()
        .expect("not provided")
        .config();
    let vector_path = &config.rag.vector.vector_path;
    let vector_store = vector_store_from_ctx(&ctx, vector_path).await?;

    // Check collection exists
    if !vector_store.collection_exists(&scoped_collection).await? {
        return Err(HttpError::from(AppError::NotFound(
            format!("Collection '{}' not found", payload.collection).into(),
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
    let embeddings = embed_for_rag(&ctx, std::slice::from_ref(&payload.query)).await?;
    let query_embedding = embeddings
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Internal("No embedding generated".to_string()))?;

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
    #[cfg_attr(not(feature = "local-embeddings"), allow(unused_mut))]
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

    // Apply reranking if requested. Remote embeddings skip the cross-encoder.
    #[cfg(feature = "local-embeddings")]
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
    #[cfg(not(feature = "local-embeddings"))]
    let reranked = false;

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
pub async fn delete_collection(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Json(payload): Json<RagDeleteCollectionRequest>,
) -> Result<Json<RagDeleteCollectionResponse>> {
    // Validate input
    if payload.collection.is_empty() {
        return Err(HttpError::from(AppError::InvalidInput(
            "Collection name required".to_string(),
        )));
    }

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let allowlist_store = allowlist::TenantAllowlistStore::new(&pool);
    if !allowlist_store
        .is_rag_source_allowed(&claims.sub, &payload.collection)
        .await?
    {
        return Err(HttpError::from(AppError::Auth(
            format!(
                "RAG source '{}' is not allowed for this tenant",
                payload.collection
            )
            .into(),
        )));
    }

    // Scope collection to user for isolation
    let scoped_collection = user_scoped_collection(&claims.sub, &payload.collection);

    let config = ctx
        .get::<crate::overlay::AresConfigManager>()
        .expect("not provided")
        .config();
    let vector_path = &config.rag.vector.vector_path;
    let vector_store = vector_store_from_ctx(&ctx, vector_path).await?;

    // Check collection exists
    if !vector_store.collection_exists(&scoped_collection).await? {
        return Err(HttpError::from(AppError::NotFound(
            format!("Collection '{}' not found", payload.collection).into(),
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
pub async fn list_collections(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
) -> Result<Json<Vec<ares_store::CollectionInfo>>> {
    let config = ctx
        .get::<crate::overlay::AresConfigManager>()
        .expect("not provided")
        .config();
    let vector_path = &config.rag.vector.vector_path;
    let vector_store = vector_store_from_ctx(&ctx, vector_path).await?;
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

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let allowlist_store = allowlist::TenantAllowlistStore::new(&pool);
    let db_sources = allowlist_store.list_rag_sources(&claims.sub).await?;
    let user_collections = filter_collections_by_allowed_sources(user_collections, &db_sources);

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

    #[test]
    fn test_list_collections_allowlist_filtering() {
        let collections = vec![
            ares_store::CollectionInfo {
                name: "docs".into(),
                document_count: 1,
                dimensions: 384,
            },
            ares_store::CollectionInfo {
                name: "images".into(),
                document_count: 2,
                dimensions: 384,
            },
            ares_store::CollectionInfo {
                name: "wiki".into(),
                document_count: 3,
                dimensions: 384,
            },
        ];
        let allowed_sources = vec![
            allowlist::TenantRagAllowlistItem {
                id: "allow-1".into(),
                tenant_id: "tenant-1".into(),
                rag_source: "docs".into(),
                enabled: true,
                created_at: 1,
                updated_at: 1,
            },
            allowlist::TenantRagAllowlistItem {
                id: "allow-2".into(),
                tenant_id: "tenant-1".into(),
                rag_source: "wiki".into(),
                enabled: true,
                created_at: 1,
                updated_at: 1,
            },
            allowlist::TenantRagAllowlistItem {
                id: "disabled-1".into(),
                tenant_id: "tenant-1".into(),
                rag_source: "images".into(),
                enabled: false,
                created_at: 1,
                updated_at: 1,
            },
        ];

        let filtered = filter_collections_by_allowed_sources(collections, &allowed_sources);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().any(|c| c.name == "docs"));
        assert!(filtered.iter().any(|c| c.name == "wiki"));
        assert!(!filtered.iter().any(|c| c.name == "images"));
    }

    #[test]
    fn test_list_collections_empty_allowlist_default_denies() {
        let collections = vec![ares_store::CollectionInfo {
            name: "docs".into(),
            document_count: 1,
            dimensions: 384,
        }];

        let filtered = filter_collections_by_allowed_sources(collections, &[]);
        assert!(filtered.is_empty());
    }

    #[cfg(feature = "local-embeddings")]
    #[tokio::test]
    async fn embedding_service_from_ctx_none_without_provide() {
        let ctx = cordis::Context::new_root();
        // Reuse is ctx.get / provide_arc; do not construct here (model init is heavy).
        assert!(ctx.get::<EmbeddingService>().is_none());
    }

    #[tokio::test]
    async fn vector_store_from_ctx_none_without_provide() {
        let ctx = cordis::Context::new_root();
        assert!(ctx.get::<AresVectorStore>().is_none());
    }

    #[test]
    fn ares_vector_store_impls_cordis_service() {
        fn assert_service<T: cordis::Service>() {}
        assert_service::<AresVectorStore>();
    }

    #[cfg(feature = "local-embeddings")]
    #[tokio::test(flavor = "multi_thread")]
    async fn embedding_service_from_ctx_reuses_provided_instance() {
        let ctx = cordis::Context::new_root();
        let service = Arc::new(EmbeddingService::with_default_model().expect("default model"));
        ctx.provide_arc(service.clone());
        let got = embedding_service_from_ctx(&ctx).await.expect("from ctx");
        assert!(Arc::ptr_eq(&service, &got));
    }

    #[cfg(feature = "local-embeddings")]
    #[test]
    fn embedding_service_impls_cordis_service() {
        fn assert_service<T: cordis::Service>() {}
        assert_service::<EmbeddingService>();
    }

    #[cfg(not(feature = "local-embeddings"))]
    #[tokio::test]
    async fn embed_for_rag_errors_when_llm_missing() {
        let ctx = Context::new_root();
        let err = embed_for_rag(&ctx, &[String::from("hello")])
            .await
            .expect_err("missing Llm must fail closed");
        match err {
            AppError::Configuration(msg) => {
                assert_eq!(msg, "Llm service is not provided for remote embeddings");
            }
            other => panic!("expected Configuration, got {other:?}"),
        }
    }

    #[cfg(not(feature = "local-embeddings"))]
    #[tokio::test]
    async fn embed_for_rag_llm_short_circuit_returns_vectors() {
        use ares_llm::{ConversationMessage, LLMClient, LLMResponse, Llm};
        use ares_types::types::ToolDefinition;
        use async_trait::async_trait;
        use cordis::EventsService;

        struct DummyEmbedClient;

        #[async_trait]
        impl LLMClient for DummyEmbedClient {
            async fn generate(&self, _prompt: &str) -> ares_types::types::Result<String> {
                Err(AppError::Internal("embed-only mock".into()))
            }
            async fn generate_with_system(
                &self,
                _system: &str,
                _prompt: &str,
            ) -> ares_types::types::Result<String> {
                Err(AppError::Internal("embed-only mock".into()))
            }
            async fn generate_with_history(
                &self,
                _messages: &[(String, String)],
            ) -> ares_types::types::Result<LLMResponse> {
                Err(AppError::Internal("embed-only mock".into()))
            }
            async fn generate_with_tools(
                &self,
                _prompt: &str,
                _tools: &[ToolDefinition],
            ) -> ares_types::types::Result<LLMResponse> {
                Err(AppError::Internal("embed-only mock".into()))
            }
            async fn generate_with_tools_and_history(
                &self,
                _messages: &[ConversationMessage],
                _tools: &[ToolDefinition],
            ) -> ares_types::types::Result<LLMResponse> {
                Err(AppError::Internal("embed-only mock".into()))
            }
            async fn stream(
                &self,
                _prompt: &str,
            ) -> ares_types::types::Result<
                Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>,
            > {
                Err(AppError::Internal("embed-only mock".into()))
            }
            async fn stream_with_system(
                &self,
                _system: &str,
                _prompt: &str,
            ) -> ares_types::types::Result<
                Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>,
            > {
                Err(AppError::Internal("embed-only mock".into()))
            }
            async fn stream_with_history(
                &self,
                _messages: &[(String, String)],
            ) -> ares_types::types::Result<
                Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>,
            > {
                Err(AppError::Internal("embed-only mock".into()))
            }
            fn model_name(&self) -> &str {
                "embed-mock"
            }
        }

        let ctx = Context::new_root();
        ctx.provide(Llm::from_client(Arc::new(DummyEmbedClient)));
        let events = ctx.provide(EventsService::new());
        events.on_waterfall(
            cordis::events_catalog::ev::LLM_EMBED.to_string(),
            |_payload, _next| async move { Ok(serde_json::json!({ "embeddings": [[1.0, 2.0]] })) },
        );
        let out = embed_for_rag(&ctx, &[String::from("hi")])
            .await
            .expect("short-circuit embed");
        assert_eq!(out, vec![vec![1.0, 2.0]]);
    }
}
