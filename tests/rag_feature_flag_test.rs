//! Tests for RAG feature flag behavior.
//!
//! HTTP integration checks require `local-embeddings` and `ares-vector` because
//! RAG routes are only compiled when both features are enabled.

#![cfg(all(feature = "postgres", feature = "http"))]

use ares_http::overlay::RagConfig;
use ares_types::types::{Document, DocumentMetadata, RagIngestRequest, RagSearchRequest};
use chrono::Utc;

#[test]
fn rag_config_disabled_by_default() {
    assert!(!RagConfig::default().vector.enabled);
}

#[test]
fn rag_request_and_document_types_are_available() {
    let _ingest = RagIngestRequest {
        collection: "test_collection".to_string(),
        content: "Test content".to_string(),
        title: Some("Test Document".to_string()),
        source: Some("test".to_string()),
        tags: vec!["test".to_string()],
        chunking_strategy: None,
    };
    let _search = RagSearchRequest {
        collection: "test_collection".to_string(),
        query: "test query".to_string(),
        limit: 5,
        strategy: None,
        threshold: 0.0,
        rerank: false,
        reranker_model: None,
    };
    let _doc = mock_document();
}

fn mock_document() -> Document {
    Document {
        id: "test-doc-1".to_string(),
        content: "Test content".to_string(),
        metadata: DocumentMetadata {
            title: "Test Document".to_string(),
            source: "test-source".to_string(),
            created_at: Utc::now(),
            tags: vec!["test".to_string()],
        },
        embedding: None,
    }
}
