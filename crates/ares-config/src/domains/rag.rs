use serde::{Deserialize, Serialize};

// ============= RAG Configuration =============
// RAG (Retrieval Augmented Generation) configuration.
// =========== Vector Store ===========
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RAGVectorConfig {
    /// Enable RAG feature flag (disabled by default for safety)
    pub enabled: bool,
    /// Available models: bge-small-en-v1.5, bge-base-en-v1.5, bge-large-en-v1.5,
    /// all-minilm-l6-v2, all-minilm-l12-v2, nomic-embed-text-v1.5, etc.
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,
    /// Enable sparse embeddings for hybrid search (default: false)
    #[serde(default)]
    pub sparse_embeddings: bool,
    /// Sparse embedding model (default: "splade-pp-en-v1")
    #[serde(default = "default_sparse_model")]
    pub sparse_model: String,
    /// Path to store vector data (default: "./data/vectors")
    #[serde(default = "default_vector_path")]
    pub vector_path: String,
}

// =========== Chunking ===========
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RagChunkingConfig {
    /// Chunking strategy: "word" (default), "semantic", "character"
    #[serde(default = "default_chunking_strategy")]
    pub chunking_strategy: String,
    /// Size of text chunks for indexing (default: 200 words or 500 chars for semantic).
    #[serde(default = "default_chunk_size")]
    pub chunk_size: usize,
    /// Overlap between consecutive chunks (default: 50).
    #[serde(default = "default_chunk_overlap")]
    pub chunk_overlap: usize,
    /// Minimum chunk size to keep (default: 20 chars).
    #[serde(default = "default_min_chunk_size")]
    pub min_chunk_size: usize,
}

// =========== Search ===========
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RagSearchConfig {
    /// Default search strategy: "semantic" (default), "bm25", "fuzzy", "hybrid"
    #[serde(default = "default_search_strategy")]
    pub search_strategy: String,
    /// Default number of results to return (default: 10)
    #[serde(default = "default_search_limit")]
    pub search_limit: usize,
    /// Default similarity threshold (default: 0.0)
    #[serde(default)]
    pub search_threshold: f32,
    /// Hybrid search weights (semantic, bm25, fuzzy) - sum should be 1.0
    #[serde(default)]
    pub hybrid_weights: Option<HybridWeightsConfig>,
}

// =========== Reranking ===========
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RagRerankingConfig {
    /// Enable reranking by default (default: false)
    #[serde(default)]
    pub rerank_enabled: bool,
    /// Reranker model: "bge-reranker-base" (default), "bge-reranker-v2-m3",
    /// "jina-reranker-v1-turbo-en", "jina-reranker-v2-base-multilingual"
    #[serde(default = "default_reranker_model")]
    pub reranker_model: String,
    /// Weight for combining rerank and retrieval scores (default: 0.6)
    #[serde(default = "default_rerank_weight")]
    pub rerank_weight: f32,
}
/// RAG Configuration Wrapper
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RagConfig {
    /// Vector store configuration for vector embeddings and retrieval
    pub vector: RAGVectorConfig,
    /// Text chunking strategy for document indexing
    pub chunking: RagChunkingConfig,
    /// Search strategy and configuration
    pub search: RagSearchConfig,
    /// Reranking configuration
    pub rerank: RagRerankingConfig,
}
#[derive(Debug, Clone, Deserialize, Serialize)]

pub struct HybridWeightsConfig {
    /// Weight for semantic search (default: 0.5)
    #[serde(default = "default_semantic_weight")]
    pub semantic: f32,
    /// Weight for BM25 search (default: 0.3)
    #[serde(default = "default_bm25_weight")]
    pub bm25: f32,
    /// Weight for fuzzy search (default: 0.2)
    #[serde(default = "default_fuzzy_weight")]
    pub fuzzy: f32,
}

impl Default for HybridWeightsConfig {
    fn default() -> Self {
        Self {
            semantic: 0.5,
            bm25: 0.3,
            fuzzy: 0.2,
        }
    }
}

impl Default for RAGVectorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            embedding_model: default_embedding_model(),
            sparse_embeddings: false,
            sparse_model: default_sparse_model(),
            vector_path: default_vector_path(),
        }
    }
}

impl Default for RagChunkingConfig {
    fn default() -> Self {
        Self {
            chunking_strategy: default_chunking_strategy(),
            chunk_size: default_chunk_size(),
            chunk_overlap: default_chunk_overlap(),
            min_chunk_size: default_min_chunk_size(),
        }
    }
}

impl Default for RagSearchConfig {
    fn default() -> Self {
        Self {
            search_strategy: default_search_strategy(),
            search_limit: default_search_limit(),
            search_threshold: 0.0,
            hybrid_weights: None,
        }
    }
}

impl Default for RagRerankingConfig {
    fn default() -> Self {
        Self {
            rerank_enabled: false,
            reranker_model: default_reranker_model(),
            rerank_weight: default_rerank_weight(),
        }
    }
}

fn default_semantic_weight() -> f32 {
    0.5
}

fn default_bm25_weight() -> f32 {
    0.3
}

fn default_fuzzy_weight() -> f32 {
    0.2
}

fn default_vector_path() -> String {
    "./data/vectors".to_string()
}

fn default_embedding_model() -> String {
    "bge-small-en-v1.5".to_string()
}

fn default_sparse_model() -> String {
    "splade-pp-en-v1".to_string()
}

fn default_chunking_strategy() -> String {
    "word".to_string()
}

fn default_chunk_size() -> usize {
    200
}

fn default_chunk_overlap() -> usize {
    50
}

fn default_min_chunk_size() -> usize {
    20
}

fn default_search_strategy() -> String {
    "semantic".to_string()
}

fn default_search_limit() -> usize {
    10
}

fn default_reranker_model() -> String {
    "bge-reranker-base".to_string()
}

fn default_rerank_weight() -> f32 {
    0.6
}
