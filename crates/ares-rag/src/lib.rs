//! Retrieval Augmented Generation (RAG) Pipeline
//!
//! This module provides the core RAG pipeline components for enhancing LLM responses
//! with relevant context from your document collections.
//!
//! # Module Structure
//!
//! - `rag::embeddings` - Dense embedding models (fastembed, 38+ models) **[requires `local-embeddings` feature]**
//! - [`rag::search`](crate::search) - Search strategies (semantic, BM25, fuzzy, hybrid)
//! - `rag::reranker` - Cross-encoder reranking for improved relevance **[requires `local-embeddings` feature]**
//! - [`rag::chunker`](crate::chunker) - Text chunking for document processing
//! - [`rag::cache`](crate::cache) - Embedding cache for avoiding recomputation
//! - [`rag::llm_embed`](crate::llm_embed) - Remote embeddings via `Llm` **[always compiled]**
//!
//! # Feature Flags
//!
//! The `local-embeddings` feature enables ONNX-based local embedding and reranking models.
//! This feature is optional because the ONNX runtime (`ort`) can have build issues on some platforms,
//! particularly Windows with certain MSVC versions.
//!
//! **Note:** The `local-embeddings` feature is NOT supported on Windows MSVC due to linker errors
//! in `ort-sys`. Use WSL, Linux, or macOS for local embeddings, or use remote embedding APIs.
//!
//! Without `local-embeddings`, you can still use:
//! - [`embed_with_llm`] when an [`ares_llm::Llm`] service is on the context
//!   (`ctx.get::<Llm>()` then `llm.embed`; never import genai)
//! - Remote embedding APIs (OpenAI embeddings, Ollama embeddings, etc.)
//! - The chunker and search modules
//! - The cache module (if you have embeddings from elsewhere)
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

// Compile-time error for unsupported platform + feature combination
#[cfg(all(
    feature = "local-embeddings",
    target_os = "windows",
    target_env = "msvc"
))]
compile_error!(
    "The `local-embeddings` feature is not supported on Windows MSVC due to ort-sys linker errors. \
    Please use one of the following alternatives:\n\
    1. Use WSL (Windows Subsystem for Linux)\n\
    2. Use remote embedding APIs (OpenAI, Ollama, etc.)\n\
    3. Build on Linux or macOS\n\
    4. Disable this feature: cargo build --no-default-features --features \"...\""
);

pub mod config;
pub use config::{
    HybridWeightsConfig, RagChunkingConfig, RagConfig, RagRerankingConfig, RagSearchConfig,
    RAGVectorConfig,
};

pub mod cache;
pub mod chunker;
#[cfg(feature = "local-embeddings")]
pub mod embeddings;
pub mod llm_embed;
pub use llm_embed::embed_with_llm;
#[cfg(feature = "local-embeddings")]
pub mod reranker;
pub mod search;

#[cfg(test)]
mod lib_tests {
    use crate::chunker::ChunkingStrategy;
    use std::str::FromStr;

    #[test]
    fn chunking_strategy_from_str() {
        let strategy = ChunkingStrategy::from_str("semantic").expect("parse");
        assert_eq!(strategy, ChunkingStrategy::Semantic);
    }

    #[test]
    fn search_module_is_linked() {
        let _ = std::any::type_name::<crate::search::SearchStrategy>();
    }

    #[test]
    fn llm_embed_is_linked() {
        let _ = crate::embed_with_llm;
    }
}
