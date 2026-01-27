//! Retrieval Augmented Generation (RAG) Pipeline
//!
//! This module provides the core RAG pipeline components for enhancing LLM responses
//! with relevant context from your document collections.
//!
//! # Module Structure
//!
//! - [`rag::embeddings`](crate::rag::embeddings) - Dense embedding models (fastembed, 38+ models)
//! - [`rag::search`](crate::rag::search) - Search strategies (semantic, BM25, fuzzy, hybrid)
//! - [`rag::reranker`](crate::rag::reranker) - Cross-encoder reranking for improved relevance
//! - [`rag::chunker`](crate::rag::chunker) - Text chunking for document processing
//!
//! # RAG Pipeline
//!
//! The typical RAG pipeline flow:
//!
//! 1. **Ingestion** - Documents are chunked and embedded
//! 2. **Storage** - Embeddings stored in vector database
//! 3. **Retrieval** - Query embedded, similar chunks retrieved
//! 4. **Reranking** - Cross-encoder reranks for relevance
//! 5. **Generation** - LLM generates response with context
//!
//! # Example
//!
//! ```ignore
//! use ares::rag::{embeddings::EmbeddingModel, chunker::Chunker, search::SearchStrategy};
//!
//! // Embed a document
//! let embedder = EmbeddingModel::new("BAAI/bge-small-en-v1.5")?;
//! let chunker = Chunker::new(512, 50);  // chunk_size, overlap
//!
//! let chunks = chunker.chunk(&document_text);
//! let embeddings = embedder.embed_batch(&chunks).await?;
//!
//! // Search
//! let query_embedding = embedder.embed(&query).await?;
//! let results = vector_store.search("my_collection", query_embedding, 10).await?;
//! ```
//!
//! # Embedding Models
//!
//! Supports 38+ models via fastembed. Popular choices:
//! - `BAAI/bge-small-en-v1.5` - Fast, good quality (default)
//! - `BAAI/bge-base-en-v1.5` - Higher quality, slower
//! - `sentence-transformers/all-MiniLM-L6-v2` - Lightweight

pub mod chunker;
pub mod embeddings;
pub mod reranker;
pub mod search;
