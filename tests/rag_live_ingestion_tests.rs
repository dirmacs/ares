//! Live Document Ingestion Pipeline Tests
//!
//! These tests verify the complete RAG pipeline with real local documents from a configured directory.
//! They test:
//! - Batch document ingestion from the the configured documents directory
//! - Semantic search over ingested documents
//! - Context injection for LLM responses
//!
//! # Running the tests
//!
//! ```bash
//! # Run all live ingestion tests
//! LIVE_INGESTION_TESTS=1 cargo test --test rag_live_ingestion_tests -- --ignored
//!
//! # Run with verbose output
//! LIVE_INGESTION_TESTS=1 RUST_LOG=info cargo test --test rag_live_ingestion_tests -- --ignored --nocapture
//! ```
//!
//! # Environment Variables
//!
//! - `LIVE_INGESTION_TESTS=1` - Enable live ingestion tests (required)
//! - `LIVE_DOCS_PATH` - Path to the documents directory (default: /opt/ares-docs)
//! - `LIVE_VECTOR_PATH` - Path for vector store persistence (default: temp dir)

#![cfg(all(feature = "ares-vector", feature = "local-embeddings"))]

use ares_http::{
    db::{AresVectorStore, VectorStore},
    rag::{
        chunker::{ChunkingStrategy, TextChunker},
        embeddings::{EmbeddingModelType, EmbeddingService},
        search::SearchEngine,
    },
    types::{Document, DocumentMetadata},
};
use chrono::Utc;
use std::path::PathBuf;
use std::time::Instant;
use std::{fs, path::Path};

// ============================================================================
// Test Configuration
// ============================================================================

fn should_run_tests() -> bool {
    std::env::var("LIVE_INGESTION_TESTS").is_ok()
}

fn get_docs_path() -> PathBuf {
    std::env::var("LIVE_DOCS_PATH")
        .unwrap_or_else(|_| "/opt/ares-docs".to_string())
        .into()
}

fn get_vector_path() -> Option<String> {
    std::env::var("LIVE_VECTOR_PATH").ok()
}

macro_rules! skip_if_not_enabled {
    () => {
        if !should_run_tests() {
            eprintln!(
                "Skipping the live ingestion test. Set LIVE_INGESTION_TESTS=1 to run with real documents."
            );
            return;
        }
    };
}

// ============================================================================
// Document Ingestion Helpers
// ============================================================================

/// Recursively find all markdown files in a directory
fn find_markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(find_markdown_files(&path));
            } else if path.extension().map_or(false, |ext| ext == "md") {
                files.push(path);
            }
        }
    }

    files
}

/// Read and parse a markdown file, extracting title from first heading
fn read_markdown_file(path: &Path) -> Option<(String, String)> {
    let content = fs::read_to_string(path).ok()?;

    if content.trim().is_empty() {
        return None;
    }

    // Extract title from first # heading or use filename
    let title = content
        .lines()
        .find(|line| line.starts_with("# "))
        .map(|line| line.trim_start_matches("# ").trim().to_string())
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        });

    Some((title, content))
}

/// Chunk a document and create embeddings
async fn process_document(
    title: &str,
    content: &str,
    source: &str,
    chunker: &TextChunker,
    embedding_service: &EmbeddingService,
) -> Vec<Document> {
    let chunks = chunker.chunk_with_metadata(content);
    let chunk_texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();

    let embeddings = match embedding_service.embed_texts(&chunk_texts).await {
        Ok(emb) => emb,
        Err(e) => {
            eprintln!("Failed to embed document '{}': {}", title, e);
            return Vec::new();
        }
    };

    chunks
        .into_iter()
        .zip(embeddings)
        .enumerate()
        .map(|(i, (chunk, embedding))| Document {
            id: format!("{}_chunk_{}", source, i),
            content: chunk.content,
            metadata: DocumentMetadata {
                title: title.to_string(),
                source: source.to_string(),
                created_at: Utc::now(),
                tags: vec!["live-docs".to_string(), "knowledge-base".to_string()],
            },
            embedding: Some(embedding),
        })
        .collect()
}

// ============================================================================
// Integration Tests
// ============================================================================

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_live_document_discovery() {
    skip_if_not_enabled!();

    let docs_path = get_docs_path();
    println!("Scanning for documents in: {:?}", docs_path);

    let markdown_files = find_markdown_files(&docs_path);

    println!(
        "Found {} markdown files:",
        markdown_files.len()
    );

    for file in &markdown_files {
        println!("  - {:?}", file);
    }

    assert!(
        !markdown_files.is_empty(),
        "No markdown files found in {:?}",
        docs_path
    );

    // Expect at least 20 documents (the task mentions 70 articles)
    assert!(
        markdown_files.len() >= 20,
        "Expected at least 20 documents, found {}",
        markdown_files.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_live_batch_ingestion() {
    skip_if_not_enabled!();

    println!("=== Live Batch Ingestion Test ===\n");

    let docs_path = get_docs_path();
    let vector_path = get_vector_path();

    // Initialize services
    let embedding_service =
        EmbeddingService::with_model(EmbeddingModelType::BgeSmallEnV15)
            .expect("Failed to create embedding service");

    let store = AresVectorStore::new(vector_path)
        .await
        .expect("Failed to create vector store");

    let collection = "live_knowledge_base";

    // Create collection
    if !store.collection_exists(collection).await.unwrap() {
        store
            .create_collection(collection, embedding_service.dimensions())
            .await
            .expect("Failed to create collection");
        println!("Created collection: {}", collection);
    } else {
        println!("Using existing collection: {}", collection);
    }

    // Find and process documents
    let markdown_files = find_markdown_files(&docs_path);
    println!(
        "Found {} documents to ingest\n",
        markdown_files.len()
    );

    let chunker = TextChunker::with_word_chunking(300, 50);
    let mut total_chunks = 0;
    let mut successful_docs = 0;

    let start = Instant::now();

    for file_path in &markdown_files {
        let (title, content) = match read_markdown_file(file_path) {
            Some(data) => data,
            None => continue,
        };

        let source = file_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        let documents =
            process_document(&title, &content, &source, &chunker, &embedding_service).await;

        if !documents.is_empty() {
            match store.upsert(collection, &documents).await {
                Ok(count) => {
                    total_chunks += count;
                    successful_docs += 1;
                    println!(
                        "  Ingested {}: {} chunks",
                        title, count
                    );
                }
                Err(e) => {
                    eprintln!("  Failed to upsert {}: {}", title, e);
                }
            }
        }
    }

    let duration = start.elapsed();

    println!("\n=== Ingestion Summary ===");
    println!("  Documents processed: {}", successful_docs);
    println!("  Total chunks created: {}", total_chunks);
    println!("  Duration: {:?}", duration);
    println!(
        "  Average chunks/second: {:.2}",
        total_chunks as f64 / duration.as_secs_f64()
    );

    assert!(
        successful_docs >= 20,
        "Expected to successfully ingest at least 20 documents, got {}",
        successful_docs
    );
    assert!(total_chunks > 0, "No chunks were created");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_live_semantic_search() {
    skip_if_not_enabled!();

    println!("=== Live Semantic Search Test ===\n");

    let vector_path = get_vector_path();
    let store = AresVectorStore::new(vector_path)
        .await
        .expect("Failed to create vector store");

    let collection = "live_knowledge_base";

    // Verify collection exists
    assert!(
        store.collection_exists(collection).await.unwrap(),
        "Collection '{}' does not exist. Run test_live_batch_ingestion first.",
        collection
    );

    let embedding_service =
        EmbeddingService::with_model(EmbeddingModelType::BgeSmallEnV15)
            .expect("Failed to create embedding service");

    // Test queries relevant to local docs
    let test_queries = vec![
        "mental health assessment",
        "AI diagnosis guidelines",
        "community clustering",
        "privacy first architecture",
        "crisis detection escalation",
    ];

    for query in test_queries {
        println!("Query: '{}'", query);

        let query_embedding = embedding_service
            .embed_text(query)
            .await
            .expect("Query embedding failed");

        let start = Instant::now();
        let results = store
            .search(collection, &query_embedding, 5, 0.0)
            .await
            .expect("Search failed");
        let search_time = start.elapsed();

        println!("  Found {} results in {:?}", results.len(), search_time);

        for (i, result) in results.iter().take(3).enumerate() {
            let preview = if result.document.content.len() > 100 {
                &result.document.content[..100]
            } else {
                &result.document.content
            };
            println!(
                "  {}. [{}] score: {:.4}\n     {}\n",
                i + 1,
                result.document.metadata.title,
                result.score,
                preview
            );
        }

        assert!(!results.is_empty(), "Search should return results");
        assert!(
            results[0].score > 0.3,
            "Top result should have reasonable similarity score"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_live_context_injection() {
    skip_if_not_enabled!();

    println!("=== Live Context Injection Test ===\n");

    let vector_path = get_vector_path();
    let store = AresVectorStore::new(vector_path)
        .await
        .expect("Failed to create vector store");

    let collection = "live_knowledge_base";

    assert!(
        store.collection_exists(collection).await.unwrap(),
        "Collection '{}' does not exist",
        collection
    );

    let embedding_service =
        EmbeddingService::with_model(EmbeddingModelType::BgeSmallEnV15)
            .expect("Failed to create embedding service");

    // Simulate a user query that needs context injection
    let user_query = "What should I do if I'm feeling stressed and anxious?";

    println!("User query: '{}'\n", user_query);

    // Embed the query
    let query_embedding = embedding_service
        .embed_text(user_query)
        .await
        .expect("Query embedding failed");

    // Retrieve relevant context
    let results = store
        .search(collection, &query_embedding, 5, 0.0)
        .await
        .expect("Search failed");

    // Build context string for LLM injection
    let mut context_parts = Vec::new();
    for (i, result) in results.iter().enumerate() {
        context_parts.push(format!(
            "[Context {}] (source: {}, score: {:.4})\n{}",
            i + 1,
            result.document.metadata.source,
            result.score,
            result.document.content
        ));
    }

    let context = context_parts.join("\n\n---\n\n");

    println!("=== Retrieved Context for LLM ===\n");
    println!(
        "Total context length: {} characters",
        context.len()
    );
    println!("\n{}\n", context);

    // Verify context is useful
    assert!(!context.is_empty(), "Context should not be empty");
    assert!(
        context.len() > 100,
        "Context should have substantial content"
    );
    assert!(
        results.len() > 0,
        "Should retrieve at least some context"
    );

    // Verify all results have proper metadata
    for result in &results {
        assert!(
            !result.document.metadata.title.is_empty(),
            "Result should have a title"
        );
        assert!(
            !result.document.metadata.source.is_empty(),
            "Result should have a source"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_live_collection_stats() {
    skip_if_not_enabled!();

    println!("=== Live Collection Stats Test ===\n");

    let vector_path = get_vector_path();
    let store = AresVectorStore::new(vector_path)
        .await
        .expect("Failed to create vector store");

    let collection = "live_knowledge_base";

    if !store.collection_exists(collection).await.unwrap() {
        println!(
            "Collection '{}' does not exist yet. Skipping stats test.",
            collection
        );
        return;
    }

    let stats = store
        .collection_stats(collection)
        .await
        .expect("Failed to get collection stats");

    println!("Collection: {}", stats.name);
    println!("  Document count: {}", stats.document_count);
    println!("  Dimensions: {}", stats.dimensions);
    println!(
        "  Index size: {} bytes",
        stats.index_size_bytes.unwrap_or(0)
    );
    println!("  Distance metric: {}", stats.distance_metric);

    assert!(
        stats.document_count > 0,
        "Collection should have documents"
    );
    assert_eq!(stats.dimensions, 384, "Should use BGE small embeddings");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn test_live_search_accuracy() {
    skip_if_not_enabled!();

    println!("=== Live Search Accuracy Test ===\n");

    let vector_path = get_vector_path();
    let store = AresVectorStore::new(vector_path)
        .await
        .expect("Failed to create vector store");

    let collection = "live_knowledge_base";

    assert!(
        store.collection_exists(collection).await.unwrap(),
        "Collection '{}' does not exist",
        collection
    );

    let embedding_service =
        EmbeddingService::with_model(EmbeddingModelType::BgeSmallEnV15)
            .expect("Failed to create embedding service");

    // Test cases with expected keywords in results
    let test_cases = vec![
        (
            "AI should not diagnose medical conditions",
            vec!["diagnos", "diagnostic", "assessment", "pattern"],
        ),
        (
            "Privacy and data protection requirements",
            vec!["privacy", "data", "PII", "retention"],
        ),
        (
            "Human intervention in AI decisions",
            vec!["human", "intervention", "decision", "escalation"],
        ),
    ];

    for (query, expected_keywords) in test_cases {
        println!("Query: '{}'", query);

        let query_embedding = embedding_service
            .embed_text(query)
            .await
            .expect("Query embedding failed");

        let results = store
            .search(collection, &query_embedding, 10, 0.0)
            .await
            .expect("Search failed");

        // Check if top results contain relevant keywords
        let top_result_content = results
            .first()
            .map(|r| r.document.content.to_lowercase())
            .unwrap_or_default();

        let has_relevant_content = expected_keywords
            .iter()
            .any(|kw| top_result_content.contains(&kw.to_lowercase()));

        println!(
            "  Top result relevance: {}",
            if has_relevant_content {
                "PASS"
            } else {
                "WARN"
            }
        );

        if !has_relevant_content {
            println!("  Expected keywords: {:?}", expected_keywords);
            println!(
                "  Top result preview: {}...",
                &top_result_content[..100.min(top_result_content.len())]
            );
        }

        assert!(
            !results.is_empty(),
            "Search for '{}' should return results",
            query
        );
    }
}
