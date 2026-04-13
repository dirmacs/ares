//! Semantic Search API Integration Tests
//!
//! Tests semantic search functionality.

#![cfg(all(feature = "ares-vector", feature = "local-embeddings"))]

use ares::{
    db::{AresVectorStore, VectorStore},
    rag::embeddings::{EmbeddingModelType, EmbeddingService},
    types::{Document, DocumentMetadata},
};
use chrono::Utc;
use std::sync::Arc;

fn should_run() -> bool {
    std::env::var("CI").is_ok() || std::env::var("RUN_EMBEDDING_TESTS").is_ok()
}

macro_rules! skip_if_not_enabled {
    () => {
        if !should_run() {
            eprintln!("Skipping - set CI=1 or RUN_EMBEDDING_TESTS=1");
            return;
        }
    };
}

#[tokio::test]
async fn test_semantic_search_empty_collection_returns_empty() {
    skip_if_not_enabled!();

    let temp_dir = tempfile::TempDir::new().unwrap();
    let store = AresVectorStore::new(Some(temp_dir.path().to_string_lossy().to_string()))
        .await
        .unwrap();

    store.create_collection("test_empty", 384).await.unwrap();

    let svc = Arc::new(EmbeddingService::with_model(EmbeddingModelType::default()).unwrap());
    let embedding = svc.embed_text("test query").await.unwrap();

    let results: Vec<ares::types::SearchResult> = store
        .search("test_empty", &embedding, 10, 0.0)
        .await
        .unwrap();

    assert!(results.is_empty());
}

#[tokio::test]
async fn test_semantic_search_returns_relevant_results() {
    skip_if_not_enabled!();

    let temp_dir = tempfile::TempDir::new().unwrap();
    let store = AresVectorStore::new(Some(temp_dir.path().to_string_lossy().to_string()))
        .await
        .unwrap();

    let svc = Arc::new(EmbeddingService::with_model(EmbeddingModelType::default()).unwrap());
    let dims = svc.dimensions();
    store.create_collection("test_docs", dims).await.unwrap();

    let docs = vec![
        Document {
            id: "rust_doc".to_string(),
            content: "Rust is a systems programming language focused on memory safety.".to_string(),
            embedding: None,
            metadata: DocumentMetadata { title: "Rust".to_string(), source: "docs/rust.md".to_string(), created_at: Utc::now(), tags: vec!["rust".to_string()] },
        },
        Document {
            id: "python_doc".to_string(),
            content: "Python is a high-level language for data science and ML.".to_string(),
            embedding: None,
            metadata: DocumentMetadata { title: "Python".to_string(), source: "docs/python.md".to_string(), created_at: Utc::now(), tags: vec!["python".to_string()] },
        },
    ];

    let texts: Vec<String> = docs.iter().map(|d| d.content.clone()).collect();
    let embeddings = svc.embed_texts(&texts).await.unwrap();
    let docs_with_emb: Vec<Document> = docs.into_iter().zip(embeddings.into_iter()).map(|(mut d, e)| { d.embedding = Some(e); d }).collect();
    store.upsert("test_docs", &docs_with_emb).await.unwrap();

    let rust_query = svc.embed_text("Tell me about Rust").await.unwrap();
    let rust_results: Vec<ares::types::SearchResult> = store.search("test_docs", &rust_query, 2, 0.0).await.unwrap();
    assert!(!rust_results.is_empty());
    assert_eq!(rust_results[0].document.id, "rust_doc");

    let py_query = svc.embed_text("What is Python used for").await.unwrap();
    let py_results: Vec<ares::types::SearchResult> = store.search("test_docs", &py_query, 2, 0.0).await.unwrap();
    assert!(!py_results.is_empty());
    assert_eq!(py_results[0].document.id, "python_doc");
}

#[tokio::test]
async fn test_semantic_search_respects_threshold() {
    skip_if_not_enabled!();

    let temp_dir = tempfile::TempDir::new().unwrap();
    let store = AresVectorStore::new(Some(temp_dir.path().to_string_lossy().to_string()))
        .await
        .unwrap();

    let svc = Arc::new(EmbeddingService::with_model(EmbeddingModelType::default()).unwrap());
    let dims = svc.dimensions();
    store.create_collection("threshold_test", dims).await.unwrap();

    let docs = vec![
        Document {
            id: "exact".to_string(),
            content: "The quick brown fox jumps over the lazy dog.".to_string(),
            embedding: None,
            metadata: DocumentMetadata::default(),
        },
        Document {
            id: "related".to_string(),
            content: "Animals like foxes are wild creatures.".to_string(),
            embedding: None,
            metadata: DocumentMetadata::default(),
        },
    ];

    let texts: Vec<String> = docs.iter().map(|d| d.content.clone()).collect();
    let embeddings = svc.embed_texts(&texts).await.unwrap();
    let docs_with_emb: Vec<Document> = docs.into_iter().zip(embeddings.into_iter()).map(|(mut d, e)| { d.embedding = Some(e); d }).collect();
    store.upsert("threshold_test", &docs_with_emb).await.unwrap();

    let query = svc.embed_text("The quick brown fox jumps over the lazy dog").await.unwrap();
    let results_high: Vec<ares::types::SearchResult> = store.search("threshold_test", &query, 10, 0.8).await.unwrap();
    let results_low: Vec<ares::types::SearchResult> = store.search("threshold_test", &query, 10, 0.0).await.unwrap();

    assert!(results_low.len() >= results_high.len(), "Lower threshold should give equal or more results");
}
