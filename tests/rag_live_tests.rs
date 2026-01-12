//! Live RAG Integration Tests
//!
//! These tests use REAL embedding models and vector stores.
//! They are **ignored by default** because they:
//! - Download embedding models (~100MB+)
//! - Require significant CPU/memory
//! - Take longer to run
//!
//! # Running the tests
//!
//! ```bash
//! # Run all RAG live tests
//! RAG_LIVE_TESTS=1 cargo test --features ares-vector --test rag_live_tests -- --ignored
//!
//! # Run with specific embedding model
//! RAG_EMBEDDING_MODEL=bge-small-en-v1.5 RAG_LIVE_TESTS=1 cargo test --features ares-vector --test rag_live_tests -- --ignored
//!
//! # Run with verbose output
//! RAG_LIVE_TESTS=1 RUST_LOG=debug cargo test --features ares-vector --test rag_live_tests -- --ignored --nocapture
//! ```
//!
//! # Environment Variables
//!
//! - `RAG_LIVE_TESTS=1` - Enable live tests (required)
//! - `RAG_EMBEDDING_MODEL` - Embedding model to use (default: bge-small-en-v1.5)
//! - `RAG_VECTOR_PATH` - Path for vector store persistence (default: temp dir)
//! - `RAG_RERANKER_MODEL` - Reranker model to use (default: bge-reranker-base)

#![cfg(feature = "ares-vector")]

use ares::{
    db::{AresVectorStore, VectorStore},
    rag::{
        chunker::TextChunker,
        embeddings::{EmbeddingModelType, EmbeddingService},
        reranker::{Reranker, RerankerConfig, RerankerModelType},
        search::{HybridWeights, SearchEngine},
    },
    types::{Document, DocumentMetadata},
};
use chrono::Utc;
use std::time::Instant;

// ============================================================================
// Test Configuration
// ============================================================================

/// Check if live tests should run
fn should_run_live_tests() -> bool {
    std::env::var("RAG_LIVE_TESTS").is_ok()
}

/// Get the embedding model from environment or use default
fn get_embedding_model() -> EmbeddingModelType {
    std::env::var("RAG_EMBEDDING_MODEL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(EmbeddingModelType::BgeSmallEnV15)
}

/// Get the reranker model from environment or use default
fn get_reranker_model() -> RerankerModelType {
    std::env::var("RAG_RERANKER_MODEL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(RerankerModelType::BgeRerankerBase)
}

/// Get vector store path or use temp directory
fn get_vector_path() -> Option<String> {
    std::env::var("RAG_VECTOR_PATH").ok()
}

/// Skip test if live tests are not enabled
macro_rules! skip_if_not_live {
    () => {
        if !should_run_live_tests() {
            eprintln!("Skipping live test. Set RAG_LIVE_TESTS=1 to run with real models.");
            return;
        }
    };
}

// ============================================================================
// Sample Data
// ============================================================================

fn sample_documents() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "rust_intro",
            "Rust is a systems programming language focused on safety, speed, and concurrency. \
             It achieves memory safety without garbage collection through its ownership system.",
        ),
        (
            "rust_ownership",
            "The ownership system in Rust ensures memory safety at compile time. Each value has \
             a single owner, and when the owner goes out of scope, the value is dropped.",
        ),
        (
            "python_intro",
            "Python is a high-level, interpreted programming language known for its simple syntax \
             and readability. It supports multiple programming paradigms including procedural, \
             object-oriented, and functional programming.",
        ),
        (
            "javascript_intro",
            "JavaScript is a versatile programming language primarily used for web development. \
             It runs in browsers and on servers via Node.js, enabling full-stack development.",
        ),
        (
            "machine_learning",
            "Machine learning is a subset of artificial intelligence that enables computers to \
             learn from data without being explicitly programmed. Common techniques include \
             supervised learning, unsupervised learning, and reinforcement learning.",
        ),
    ]
}

fn long_document() -> &'static str {
    r#"
    Retrieval-Augmented Generation (RAG) is a powerful technique that combines the strengths
    of large language models with external knowledge retrieval. Instead of relying solely on
    the knowledge encoded in model weights during training, RAG systems can access up-to-date
    information from external sources.

    The RAG pipeline typically consists of several key components:

    1. Document Ingestion: Documents are processed and split into smaller chunks that can be
       efficiently embedded and retrieved. Common chunking strategies include fixed-size chunks,
       sentence-based splitting, and semantic chunking that respects document structure.

    2. Embedding Generation: Each chunk is converted into a dense vector representation using
       an embedding model. Popular models include OpenAI's text-embedding-ada-002, sentence
       transformers like all-MiniLM-L6-v2, and BGE models from BAAI.

    3. Vector Storage: The embeddings are stored in a vector database that supports efficient
       similarity search. Options range from simple in-memory stores to distributed systems
       like Pinecone, Weaviate, Milvus, and Qdrant.

    4. Query Processing: When a user submits a query, it is embedded using the same model
       used for documents. The query embedding is then used to find the most similar document
       chunks in the vector store.

    5. Retrieval: The top-k most similar chunks are retrieved based on cosine similarity or
       other distance metrics. This step may include filtering based on metadata.

    6. Reranking (Optional): A cross-encoder model can rerank the initial results for improved
       relevance. This is more computationally expensive but often yields better results.

    7. Generation: The retrieved context is provided to the language model along with the
       original query to generate a grounded response.

    Best practices for RAG systems include:
    - Choose chunk sizes appropriate for your use case (typically 256-512 tokens)
    - Use overlap between chunks to maintain context
    - Include metadata for filtering and attribution
    - Implement hybrid search combining semantic and keyword matching
    - Consider reranking for improved precision
    - Monitor and evaluate retrieval quality regularly
    "#
}

// ============================================================================
// Embedding Tests
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_live_embedding_model_loading() {
    skip_if_not_live!();

    let model = get_embedding_model();
    println!("Loading embedding model: {:?}", model);

    let start = Instant::now();
    let service = EmbeddingService::with_model(model).expect("Failed to create embedding service");
    let load_time = start.elapsed();

    println!("Model loaded in {:?}", load_time);
    println!("Model dimensions: {}", service.dimensions());

    assert!(service.dimensions() > 0, "Dimensions should be positive");
}

#[tokio::test]
#[ignore]
async fn test_live_single_embedding() {
    skip_if_not_live!();

    let service =
        EmbeddingService::with_model(get_embedding_model()).expect("Failed to create service");

    let text = "Rust is a systems programming language.";

    let start = Instant::now();
    let embedding = service.embed_text(text).await.expect("Embedding failed");
    let embed_time = start.elapsed();

    println!("Generated embedding in {:?}", embed_time);
    println!("Embedding dimensions: {}", embedding.len());
    println!("First 5 values: {:?}", &embedding[..5.min(embedding.len())]);

    assert_eq!(embedding.len(), service.dimensions());
}

#[tokio::test]
#[ignore]
async fn test_live_batch_embeddings() {
    skip_if_not_live!();

    let service =
        EmbeddingService::with_model(get_embedding_model()).expect("Failed to create service");

    let texts: Vec<String> = sample_documents()
        .iter()
        .map(|(_, content)| content.to_string())
        .collect();

    let start = Instant::now();
    let embeddings = service
        .embed_texts(&texts)
        .await
        .expect("Batch embedding failed");
    let embed_time = start.elapsed();

    println!(
        "Generated {} embeddings in {:?}",
        embeddings.len(),
        embed_time
    );
    println!(
        "Average time per embedding: {:?}",
        embed_time / embeddings.len() as u32
    );

    assert_eq!(embeddings.len(), texts.len());
    for emb in &embeddings {
        assert_eq!(emb.len(), service.dimensions());
    }
}

#[tokio::test]
#[ignore]
async fn test_live_embedding_similarity() {
    skip_if_not_live!();

    let service =
        EmbeddingService::with_model(get_embedding_model()).expect("Failed to create service");

    let texts = vec![
        "Rust programming language",
        "Rust is a systems language",
        "Python programming language",
        "Cooking recipes for dinner",
    ];

    let embeddings = service.embed_texts(&texts).await.expect("Embedding failed");

    // Calculate cosine similarities
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (norm_a * norm_b)
    }

    let sim_rust_rust = cosine_similarity(&embeddings[0], &embeddings[1]);
    let sim_rust_python = cosine_similarity(&embeddings[0], &embeddings[2]);
    let sim_rust_cooking = cosine_similarity(&embeddings[0], &embeddings[3]);

    println!("Similarity (Rust vs Rust systems): {:.4}", sim_rust_rust);
    println!("Similarity (Rust vs Python): {:.4}", sim_rust_python);
    println!("Similarity (Rust vs Cooking): {:.4}", sim_rust_cooking);

    // Related texts should have higher similarity
    assert!(
        sim_rust_rust > sim_rust_python,
        "Rust texts should be more similar to each other"
    );
    assert!(
        sim_rust_python > sim_rust_cooking,
        "Programming languages should be more similar than cooking"
    );
}

// ============================================================================
// Vector Store Tests
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_live_vector_store_crud() {
    skip_if_not_live!();

    let store = AresVectorStore::new(get_vector_path())
        .await
        .expect("Failed to create vector store");

    let collection = format!("test_crud_{}", uuid::Uuid::new_v4());

    // Create collection
    store
        .create_collection(&collection, 384)
        .await
        .expect("Failed to create collection");
    println!("Created collection: {}", collection);

    // Verify it exists
    assert!(store.collection_exists(&collection).await.unwrap());

    // Create and insert documents
    let service =
        EmbeddingService::with_model(get_embedding_model()).expect("Failed to create service");

    let mut documents = Vec::new();
    for (id, content) in sample_documents() {
        let embedding = service.embed_text(content).await.expect("Embedding failed");
        documents.push(Document {
            id: id.to_string(),
            content: content.to_string(),
            metadata: DocumentMetadata {
                title: id.to_string(),
                source: "test".to_string(),
                created_at: Utc::now(),
                tags: vec!["test".to_string()],
            },
            embedding: Some(embedding),
        });
    }

    let count = store
        .upsert(&collection, &documents)
        .await
        .expect("Upsert failed");
    println!("Inserted {} documents", count);
    assert_eq!(count, documents.len());

    // Search
    let query_embedding = service
        .embed_text("What is Rust programming?")
        .await
        .expect("Query embedding failed");

    let results = store
        .search(&collection, &query_embedding, 3, 0.0)
        .await
        .expect("Search failed");

    println!("Search results:");
    for (i, result) in results.iter().enumerate() {
        println!(
            "  {}. {} (score: {:.4})",
            i + 1,
            result.document.id,
            result.score
        );
    }

    assert!(!results.is_empty(), "Should find some results");
    assert!(
        results[0].document.id.contains("rust"),
        "Top result should be about Rust"
    );

    // Cleanup
    store
        .delete_collection(&collection)
        .await
        .expect("Failed to delete collection");
    assert!(!store.collection_exists(&collection).await.unwrap());
    println!("Cleaned up collection");
}

// ============================================================================
// Chunking Tests
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_live_chunking_and_search() {
    skip_if_not_live!();

    let store = AresVectorStore::new(get_vector_path())
        .await
        .expect("Failed to create vector store");

    let service =
        EmbeddingService::with_model(get_embedding_model()).expect("Failed to create service");

    let collection = format!("test_chunking_{}", uuid::Uuid::new_v4());

    // Create collection
    store
        .create_collection(&collection, service.dimensions())
        .await
        .expect("Failed to create collection");

    // Chunk the long document
    let chunker = TextChunker::with_semantic_chunking(500);
    let chunks = chunker.chunk_with_metadata(long_document());

    println!("Created {} chunks from long document", chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        println!(
            "  Chunk {}: {} chars, offset {}-{}",
            i,
            chunk.content.len(),
            chunk.start_offset,
            chunk.end_offset
        );
    }

    // Embed and store chunks
    let chunk_texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
    let embeddings = service
        .embed_texts(&chunk_texts)
        .await
        .expect("Embedding failed");

    let documents: Vec<Document> = chunks
        .iter()
        .zip(embeddings)
        .enumerate()
        .map(|(i, (chunk, embedding))| Document {
            id: format!("chunk_{}", i),
            content: chunk.content.clone(),
            metadata: DocumentMetadata {
                title: format!("RAG Document - Chunk {}", i),
                source: "long_document".to_string(),
                created_at: Utc::now(),
                tags: vec!["rag".to_string(), "test".to_string()],
            },
            embedding: Some(embedding),
        })
        .collect();

    store
        .upsert(&collection, &documents)
        .await
        .expect("Upsert failed");

    // Test various queries
    let queries = [
        "What are the components of a RAG pipeline?",
        "How does embedding work?",
        "What is reranking?",
        "Best practices for chunk size",
    ];

    for query in queries {
        let query_embedding = service.embed_text(query).await.expect("Query embed failed");
        let results = store
            .search(&collection, &query_embedding, 2, 0.0)
            .await
            .expect("Search failed");

        println!("\nQuery: {}", query);
        for (i, r) in results.iter().enumerate() {
            println!(
                "  {}. {} (score: {:.4}): {}...",
                i + 1,
                r.document.id,
                r.score,
                &r.document.content[..80.min(r.document.content.len())]
            );
        }
    }

    // Cleanup
    store.delete_collection(&collection).await.ok();
}

// ============================================================================
// Search Strategy Tests
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_live_hybrid_search() {
    skip_if_not_live!();

    let store = AresVectorStore::new(get_vector_path())
        .await
        .expect("Failed to create vector store");

    let service =
        EmbeddingService::with_model(get_embedding_model()).expect("Failed to create service");

    let collection = format!("test_hybrid_{}", uuid::Uuid::new_v4());

    store
        .create_collection(&collection, service.dimensions())
        .await
        .expect("Failed to create collection");

    // Insert documents
    let mut documents = Vec::new();
    for (id, content) in sample_documents() {
        let embedding = service.embed_text(content).await.expect("Embedding failed");
        documents.push(Document {
            id: id.to_string(),
            content: content.to_string(),
            metadata: DocumentMetadata::default(),
            embedding: Some(embedding),
        });
    }

    store.upsert(&collection, &documents).await.unwrap();

    // Semantic search
    let query = "memory safety without garbage collection";
    let query_embedding = service.embed_text(query).await.unwrap();

    let semantic_results = store
        .search(&collection, &query_embedding, 5, 0.0)
        .await
        .unwrap();

    println!("Semantic search for: '{}'", query);
    for (i, r) in semantic_results.iter().enumerate() {
        println!("  {}. {} (score: {:.4})", i + 1, r.document.id, r.score);
    }

    // Build search engine for hybrid search
    let mut search_engine = SearchEngine::new();
    for doc in &documents {
        search_engine.index_document(doc);
    }

    // BM25 search
    let bm25_results = search_engine.search_bm25(query, 5);
    println!("\nBM25 search:");
    for (i, (id, score)) in bm25_results.iter().enumerate() {
        println!("  {}. {} (score: {:.4})", i + 1, id, score);
    }

    // Hybrid search
    let semantic_scores: Vec<_> = semantic_results
        .iter()
        .map(|r| (r.document.id.clone(), r.score))
        .collect();

    let hybrid_results =
        search_engine.search_hybrid(query, &semantic_scores, &HybridWeights::default(), 5);

    println!("\nHybrid search:");
    for (i, (id, score)) in hybrid_results.iter().enumerate() {
        println!("  {}. {} (score: {:.4})", i + 1, id, score);
    }

    // Cleanup
    store.delete_collection(&collection).await.ok();
}

// ============================================================================
// Reranker Tests
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_live_reranking() {
    skip_if_not_live!();

    let model = get_reranker_model();
    println!("Loading reranker model: {:?}", model);

    let config = RerankerConfig {
        model,
        show_download_progress: true,
        ..Default::default()
    };

    let reranker = Reranker::new(config);

    let query = "What programming language focuses on memory safety?";

    let candidates: Vec<(String, String, f32)> = sample_documents()
        .iter()
        .enumerate()
        .map(|(i, (id, content))| (id.to_string(), content.to_string(), 1.0 - (i as f32 * 0.1)))
        .collect();

    println!("Query: {}", query);
    println!("\nBefore reranking:");
    for (id, _, score) in &candidates {
        println!("  {} (score: {:.4})", id, score);
    }

    let start = Instant::now();
    let reranked = reranker
        .rerank(query, &candidates, Some(5))
        .await
        .expect("Reranking failed");
    let rerank_time = start.elapsed();

    println!("\nAfter reranking (took {:?}):", rerank_time);
    for result in &reranked {
        println!(
            "  {} (rerank: {:.4}, retrieval: {:.4}, final: {:.4})",
            result.id, result.rerank_score, result.retrieval_score, result.final_score
        );
    }

    // The Rust documents should be ranked higher for this query
    assert!(
        reranked[0].id.contains("rust"),
        "Top result should be about Rust"
    );
}

// ============================================================================
// End-to-End Pipeline Test
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_live_full_rag_pipeline() {
    skip_if_not_live!();

    println!("=== Full RAG Pipeline Test ===\n");

    // 1. Initialize components
    let store = AresVectorStore::new(get_vector_path())
        .await
        .expect("Failed to create vector store");

    let embedding_service =
        EmbeddingService::with_model(get_embedding_model()).expect("Failed to create embeddings");

    let reranker_config = RerankerConfig {
        model: get_reranker_model(),
        show_download_progress: true,
        ..Default::default()
    };
    let reranker = Reranker::new(reranker_config);

    let collection = format!("test_pipeline_{}", uuid::Uuid::new_v4());

    println!("Embedding model: {:?}", get_embedding_model());
    println!("Reranker model: {:?}", get_reranker_model());
    println!("Collection: {}\n", collection);

    // 2. Create collection
    store
        .create_collection(&collection, embedding_service.dimensions())
        .await
        .expect("Failed to create collection");

    // 3. Ingest documents with chunking
    let chunker = TextChunker::with_word_chunking(100, 20);

    let mut all_documents = Vec::new();
    let mut doc_id = 0;

    for (source_id, content) in sample_documents() {
        let chunks = chunker.chunk(content);
        for chunk in chunks {
            let embedding = embedding_service
                .embed_text(&chunk)
                .await
                .expect("Embedding failed");

            all_documents.push(Document {
                id: format!("{}_{}", source_id, doc_id),
                content: chunk,
                metadata: DocumentMetadata {
                    title: source_id.to_string(),
                    source: source_id.to_string(),
                    created_at: Utc::now(),
                    tags: vec!["test".to_string()],
                },
                embedding: Some(embedding),
            });
            doc_id += 1;
        }
    }

    let ingested = store
        .upsert(&collection, &all_documents)
        .await
        .expect("Upsert failed");

    println!("Ingested {} document chunks\n", ingested);

    // 4. Query pipeline
    let query = "How does Rust ensure memory safety?";
    println!("Query: {}\n", query);

    // 4a. Generate query embedding
    let query_embedding = embedding_service
        .embed_text(query)
        .await
        .expect("Query embedding failed");

    // 4b. Vector search
    let start = Instant::now();
    let search_results = store
        .search(&collection, &query_embedding, 10, 0.0)
        .await
        .expect("Search failed");
    let search_time = start.elapsed();

    println!("Vector search ({:?}):", search_time);
    for (i, r) in search_results.iter().take(5).enumerate() {
        println!("  {}. {} (score: {:.4})", i + 1, r.document.id, r.score);
    }

    // 4c. Rerank results
    let rerank_input: Vec<_> = search_results
        .iter()
        .map(|r| (r.document.id.clone(), r.document.content.clone(), r.score))
        .collect();

    let start = Instant::now();
    let reranked = reranker
        .rerank(query, &rerank_input, Some(5))
        .await
        .expect("Reranking failed");
    let rerank_time = start.elapsed();

    println!("\nAfter reranking ({:?}):", rerank_time);
    for (i, r) in reranked.iter().enumerate() {
        println!("  {}. {} (final: {:.4})", i + 1, r.id, r.final_score);
    }

    // 5. Show top context for LLM
    println!("\n=== Retrieved Context for LLM ===\n");
    for (i, r) in reranked.iter().take(3).enumerate() {
        let doc = search_results
            .iter()
            .find(|sr| sr.document.id == r.id)
            .unwrap();
        println!(
            "{}. [{}] (score: {:.4})\n{}\n",
            i + 1,
            r.id,
            r.final_score,
            doc.document.content
        );
    }

    // Cleanup
    store.delete_collection(&collection).await.ok();
    println!("=== Test Complete ===");
}

// ============================================================================
// Performance Benchmarks
// ============================================================================

#[tokio::test]
#[ignore]
async fn test_live_embedding_throughput() {
    skip_if_not_live!();

    let service =
        EmbeddingService::with_model(get_embedding_model()).expect("Failed to create service");

    // Generate test texts
    let texts: Vec<String> = (0..100)
        .map(|i| format!("This is test document number {}. It contains some text for embedding performance testing.", i))
        .collect();

    let start = Instant::now();
    let embeddings = service
        .embed_texts(&texts)
        .await
        .expect("Batch embedding failed");
    let total_time = start.elapsed();

    let throughput = texts.len() as f64 / total_time.as_secs_f64();

    println!("Embedded {} texts in {:?}", texts.len(), total_time);
    println!("Throughput: {:.2} texts/second", throughput);
    println!("Average latency: {:?}", total_time / texts.len() as u32);

    assert_eq!(embeddings.len(), texts.len());
}

#[tokio::test]
#[ignore]
async fn test_live_search_latency() {
    skip_if_not_live!();

    let store = AresVectorStore::new(get_vector_path())
        .await
        .expect("Failed to create vector store");

    let service =
        EmbeddingService::with_model(get_embedding_model()).expect("Failed to create service");

    let collection = format!("test_latency_{}", uuid::Uuid::new_v4());

    store
        .create_collection(&collection, service.dimensions())
        .await
        .unwrap();

    // Insert 1000 documents
    let mut documents = Vec::new();
    for i in 0..1000 {
        let content = format!(
            "Document {} discusses various topics including technology, science, and programming.",
            i
        );
        let embedding = service.embed_text(&content).await.unwrap();
        documents.push(Document {
            id: format!("doc_{}", i),
            content,
            metadata: DocumentMetadata::default(),
            embedding: Some(embedding),
        });
    }

    store.upsert(&collection, &documents).await.unwrap();
    println!("Inserted {} documents", documents.len());

    // Measure search latency
    let query_embedding = service.embed_text("programming technology").await.unwrap();

    let mut latencies = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        let _ = store
            .search(&collection, &query_embedding, 10, 0.0)
            .await
            .unwrap();
        latencies.push(start.elapsed());
    }

    let avg_latency = latencies.iter().sum::<std::time::Duration>() / latencies.len() as u32;
    let min_latency = latencies.iter().min().unwrap();
    let max_latency = latencies.iter().max().unwrap();

    println!("Search latency over 10 queries:");
    println!("  Average: {:?}", avg_latency);
    println!("  Min: {:?}", min_latency);
    println!("  Max: {:?}", max_latency);

    // Cleanup
    store.delete_collection(&collection).await.ok();
}
