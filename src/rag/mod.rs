//! Retrieval Augmented Generation (RAG) components.
//!
//! This module provides the core RAG pipeline components:
//! - **embeddings**: Dense embedding models (fastembed, 38+ models)
//! - **search**: Search strategies (semantic, BM25, fuzzy, hybrid)
//! - **reranker**: Cross-encoder reranking for improved relevance
//! - **chunker**: Text chunking for document processing

#![allow(missing_docs)]

pub mod chunker;
pub mod embeddings;
pub mod reranker;
pub mod search;
