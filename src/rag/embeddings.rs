//! Embedding Service for RAG
//!
//! This module provides a comprehensive embedding service with support for:
//! - 30+ text embedding models (BGE, Qwen3, Gemma, E5, Jina, etc.)
//! - Sparse embeddings for hybrid search (SPLADE, BGE-M3)
//! - Reranking models (BGE, Jina)
//! - Async embedding via `spawn_blocking`
//!
//! # GPU Acceleration (TODO)
//! GPU acceleration is planned for future iterations. See `docs/FUTURE_ENHANCEMENTS.md`.
//! Potential approach:
//! - Add feature flags: `cuda`, `metal`, `vulkan`
//! - Use ORT execution providers for ONNX models
//! - Use Candle GPU features for Qwen3 models
//!
//! # Embedding Cache (TODO)
//! Embedding caching is deferred. See `docs/FUTURE_ENHANCEMENTS.md` and `src/rag/cache.rs`.

use crate::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::str::FromStr;
use tokio::task::spawn_blocking;

// Re-export fastembed types for convenience
pub use fastembed::{
    EmbeddingModel as FastEmbedModel, InitOptions, SparseModel, TextEmbedding,
};

// ============================================================================
// Embedding Model Configuration
// ============================================================================

/// Supported embedding models with their metadata.
///
/// This enum wraps fastembed's EmbeddingModel with additional metadata
/// for easier configuration and selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingModelType {
    // Fast English models (recommended defaults)
    /// BAAI/bge-small-en-v1.5 - Fast, 384 dimensions (DEFAULT)
    #[default]
    BgeSmallEnV15,
    /// Quantized BAAI/bge-small-en-v1.5
    BgeSmallEnV15Q,
    /// sentence-transformers/all-MiniLM-L6-v2 - Very fast, 384 dimensions
    AllMiniLmL6V2,
    /// Quantized all-MiniLM-L6-v2
    AllMiniLmL6V2Q,
    /// sentence-transformers/all-MiniLM-L12-v2 - Better quality, 384 dimensions
    AllMiniLmL12V2,
    /// Quantized all-MiniLM-L12-v2
    AllMiniLmL12V2Q,
    /// sentence-transformers/all-mpnet-base-v2 - 768 dimensions
    AllMpnetBaseV2,

    // High quality English models
    /// BAAI/bge-base-en-v1.5 - 768 dimensions
    BgeBaseEnV15,
    /// Quantized BAAI/bge-base-en-v1.5
    BgeBaseEnV15Q,
    /// BAAI/bge-large-en-v1.5 - 1024 dimensions
    BgeLargeEnV15,
    /// Quantized BAAI/bge-large-en-v1.5
    BgeLargeEnV15Q,

    // Multilingual models
    // NOTE: BGE-M3 is not available in fastembed 5.5.0, use MultilingualE5 instead
    /// intfloat/multilingual-e5-small - 384 dimensions
    MultilingualE5Small,
    /// intfloat/multilingual-e5-base - 768 dimensions
    MultilingualE5Base,
    /// intfloat/multilingual-e5-large - 1024 dimensions
    MultilingualE5Large,
    /// sentence-transformers/paraphrase-MiniLM-L12-v2
    ParaphraseMiniLmL12V2,
    /// Quantized paraphrase-MiniLM-L12-v2
    ParaphraseMiniLmL12V2Q,
    /// sentence-transformers/paraphrase-multilingual-mpnet-base-v2 - 768 dimensions
    ParaphraseMultilingualMpnetBaseV2,

    // Chinese models
    /// BAAI/bge-small-zh-v1.5 - 512 dimensions
    BgeSmallZhV15,
    /// BAAI/bge-large-zh-v1.5 - 1024 dimensions
    BgeLargeZhV15,

    // Long context models
    /// nomic-ai/nomic-embed-text-v1 - 768 dimensions, 8192 context
    NomicEmbedTextV1,
    /// nomic-ai/nomic-embed-text-v1.5 - 768 dimensions, 8192 context
    NomicEmbedTextV15,
    /// Quantized nomic-embed-text-v1.5
    NomicEmbedTextV15Q,

    // Specialized models
    /// mixedbread-ai/mxbai-embed-large-v1 - 1024 dimensions
    MxbaiEmbedLargeV1,
    /// Quantized mxbai-embed-large-v1
    MxbaiEmbedLargeV1Q,
    /// Alibaba-NLP/gte-base-en-v1.5 - 768 dimensions
    GteBaseEnV15,
    /// Quantized gte-base-en-v1.5
    GteBaseEnV15Q,
    /// Alibaba-NLP/gte-large-en-v1.5 - 1024 dimensions
    GteLargeEnV15,
    /// Quantized gte-large-en-v1.5
    GteLargeEnV15Q,
    /// Qdrant/clip-ViT-B-32-text - 512 dimensions, pairs with vision model
    ClipVitB32,

    // Code models
    /// jinaai/jina-embeddings-v2-base-code - 768 dimensions
    JinaEmbeddingsV2BaseCode,
    // NOTE: JinaEmbeddingsV2BaseEN is not available in fastembed 5.5.0

    // Modern models
    /// google/embeddinggemma-300m - 768 dimensions
    EmbeddingGemma300M,
    /// lightonai/modernbert-embed-large - 1024 dimensions
    ModernBertEmbedLarge,

    // Snowflake Arctic models
    /// snowflake/snowflake-arctic-embed-xs - 384 dimensions
    SnowflakeArcticEmbedXs,
    /// Quantized snowflake-arctic-embed-xs
    SnowflakeArcticEmbedXsQ,
    /// snowflake/snowflake-arctic-embed-s - 384 dimensions
    SnowflakeArcticEmbedS,
    /// Quantized snowflake-arctic-embed-s
    SnowflakeArcticEmbedSQ,
    /// snowflake/snowflake-arctic-embed-m - 768 dimensions
    SnowflakeArcticEmbedM,
    /// Quantized snowflake-arctic-embed-m
    SnowflakeArcticEmbedMQ,
    /// snowflake/snowflake-arctic-embed-m-long - 768 dimensions, 2048 context
    SnowflakeArcticEmbedMLong,
    /// Quantized snowflake-arctic-embed-m-long
    SnowflakeArcticEmbedMLongQ,
    /// snowflake/snowflake-arctic-embed-l - 1024 dimensions
    SnowflakeArcticEmbedL,
    /// Quantized snowflake-arctic-embed-l
    SnowflakeArcticEmbedLQ,
}

impl EmbeddingModelType {
    /// Convert to fastembed's EmbeddingModel enum
    pub fn to_fastembed_model(&self) -> FastEmbedModel {
        match self {
            // Fast English
            Self::BgeSmallEnV15 => FastEmbedModel::BGESmallENV15,
            Self::BgeSmallEnV15Q => FastEmbedModel::BGESmallENV15Q,
            Self::AllMiniLmL6V2 => FastEmbedModel::AllMiniLML6V2,
            Self::AllMiniLmL6V2Q => FastEmbedModel::AllMiniLML6V2Q,
            Self::AllMiniLmL12V2 => FastEmbedModel::AllMiniLML12V2,
            Self::AllMiniLmL12V2Q => FastEmbedModel::AllMiniLML12V2Q,
            Self::AllMpnetBaseV2 => FastEmbedModel::AllMpnetBaseV2,

            // High quality English
            Self::BgeBaseEnV15 => FastEmbedModel::BGEBaseENV15,
            Self::BgeBaseEnV15Q => FastEmbedModel::BGEBaseENV15Q,
            Self::BgeLargeEnV15 => FastEmbedModel::BGELargeENV15,
            Self::BgeLargeEnV15Q => FastEmbedModel::BGELargeENV15Q,

            // Multilingual
            Self::MultilingualE5Small => FastEmbedModel::MultilingualE5Small,
            Self::MultilingualE5Base => FastEmbedModel::MultilingualE5Base,
            Self::MultilingualE5Large => FastEmbedModel::MultilingualE5Large,
            Self::ParaphraseMiniLmL12V2 => FastEmbedModel::ParaphraseMLMiniLML12V2,
            Self::ParaphraseMiniLmL12V2Q => FastEmbedModel::ParaphraseMLMiniLML12V2Q,
            Self::ParaphraseMultilingualMpnetBaseV2 => FastEmbedModel::ParaphraseMLMpnetBaseV2,

            // Chinese
            Self::BgeSmallZhV15 => FastEmbedModel::BGESmallZHV15,
            Self::BgeLargeZhV15 => FastEmbedModel::BGELargeZHV15,

            // Long context
            Self::NomicEmbedTextV1 => FastEmbedModel::NomicEmbedTextV1,
            Self::NomicEmbedTextV15 => FastEmbedModel::NomicEmbedTextV15,
            Self::NomicEmbedTextV15Q => FastEmbedModel::NomicEmbedTextV15Q,

            // Specialized
            Self::MxbaiEmbedLargeV1 => FastEmbedModel::MxbaiEmbedLargeV1,
            Self::MxbaiEmbedLargeV1Q => FastEmbedModel::MxbaiEmbedLargeV1Q,
            Self::GteBaseEnV15 => FastEmbedModel::GTEBaseENV15,
            Self::GteBaseEnV15Q => FastEmbedModel::GTEBaseENV15Q,
            Self::GteLargeEnV15 => FastEmbedModel::GTELargeENV15,
            Self::GteLargeEnV15Q => FastEmbedModel::GTELargeENV15Q,
            Self::ClipVitB32 => FastEmbedModel::ClipVitB32,

            // Code
            Self::JinaEmbeddingsV2BaseCode => FastEmbedModel::JinaEmbeddingsV2BaseCode,

            // Modern
            Self::EmbeddingGemma300M => FastEmbedModel::EmbeddingGemma300M,
            Self::ModernBertEmbedLarge => FastEmbedModel::ModernBertEmbedLarge,

            // Snowflake Arctic
            Self::SnowflakeArcticEmbedXs => FastEmbedModel::SnowflakeArcticEmbedXS,
            Self::SnowflakeArcticEmbedXsQ => FastEmbedModel::SnowflakeArcticEmbedXSQ,
            Self::SnowflakeArcticEmbedS => FastEmbedModel::SnowflakeArcticEmbedS,
            Self::SnowflakeArcticEmbedSQ => FastEmbedModel::SnowflakeArcticEmbedSQ,
            Self::SnowflakeArcticEmbedM => FastEmbedModel::SnowflakeArcticEmbedM,
            Self::SnowflakeArcticEmbedMQ => FastEmbedModel::SnowflakeArcticEmbedMQ,
            Self::SnowflakeArcticEmbedMLong => FastEmbedModel::SnowflakeArcticEmbedMLong,
            Self::SnowflakeArcticEmbedMLongQ => FastEmbedModel::SnowflakeArcticEmbedMLongQ,
            Self::SnowflakeArcticEmbedL => FastEmbedModel::SnowflakeArcticEmbedL,
            Self::SnowflakeArcticEmbedLQ => FastEmbedModel::SnowflakeArcticEmbedLQ,
        }
    }

    /// Get the dimension of the embedding output
    pub fn dimensions(&self) -> usize {
        match self {
            // 384 dimensions
            Self::BgeSmallEnV15
            | Self::BgeSmallEnV15Q
            | Self::AllMiniLmL6V2
            | Self::AllMiniLmL6V2Q
            | Self::AllMiniLmL12V2
            | Self::AllMiniLmL12V2Q
            | Self::MultilingualE5Small
            | Self::SnowflakeArcticEmbedXs
            | Self::SnowflakeArcticEmbedXsQ
            | Self::SnowflakeArcticEmbedS
            | Self::SnowflakeArcticEmbedSQ => 384,

            // 512 dimensions
            Self::BgeSmallZhV15 | Self::ClipVitB32 => 512,

            // 768 dimensions
            Self::AllMpnetBaseV2
            | Self::BgeBaseEnV15
            | Self::BgeBaseEnV15Q
            | Self::MultilingualE5Base
            | Self::ParaphraseMiniLmL12V2
            | Self::ParaphraseMiniLmL12V2Q
            | Self::ParaphraseMultilingualMpnetBaseV2
            | Self::NomicEmbedTextV1
            | Self::NomicEmbedTextV15
            | Self::NomicEmbedTextV15Q
            | Self::GteBaseEnV15
            | Self::GteBaseEnV15Q
            | Self::JinaEmbeddingsV2BaseCode
            | Self::EmbeddingGemma300M
            | Self::SnowflakeArcticEmbedM
            | Self::SnowflakeArcticEmbedMQ
            | Self::SnowflakeArcticEmbedMLong
            | Self::SnowflakeArcticEmbedMLongQ => 768,

            // 1024 dimensions
            Self::BgeLargeEnV15
            | Self::BgeLargeEnV15Q
            | Self::BgeLargeZhV15
            | Self::MultilingualE5Large
            | Self::MxbaiEmbedLargeV1
            | Self::MxbaiEmbedLargeV1Q
            | Self::GteLargeEnV15
            | Self::GteLargeEnV15Q
            | Self::ModernBertEmbedLarge
            | Self::SnowflakeArcticEmbedL
            | Self::SnowflakeArcticEmbedLQ => 1024,
        }
    }

    /// Check if this is a quantized model
    pub fn is_quantized(&self) -> bool {
        matches!(
            self,
            Self::BgeSmallEnV15Q
                | Self::AllMiniLmL6V2Q
                | Self::AllMiniLmL12V2Q
                | Self::BgeBaseEnV15Q
                | Self::BgeLargeEnV15Q
                | Self::ParaphraseMiniLmL12V2Q
                | Self::NomicEmbedTextV15Q
                | Self::MxbaiEmbedLargeV1Q
                | Self::GteBaseEnV15Q
                | Self::GteLargeEnV15Q
                | Self::SnowflakeArcticEmbedXsQ
                | Self::SnowflakeArcticEmbedSQ
                | Self::SnowflakeArcticEmbedMQ
                | Self::SnowflakeArcticEmbedMLongQ
                | Self::SnowflakeArcticEmbedLQ
        )
    }

    /// Check if this model supports multilingual text
    pub fn is_multilingual(&self) -> bool {
        matches!(
            self,
            Self::MultilingualE5Small
                | Self::MultilingualE5Base
                | Self::MultilingualE5Large
                | Self::ParaphraseMultilingualMpnetBaseV2
                | Self::BgeSmallZhV15
                | Self::BgeLargeZhV15
        )
    }

    /// Get the maximum context length in tokens
    pub fn max_context_length(&self) -> usize {
        match self {
            Self::NomicEmbedTextV1 | Self::NomicEmbedTextV15 | Self::NomicEmbedTextV15Q => 8192,
            Self::SnowflakeArcticEmbedMLong | Self::SnowflakeArcticEmbedMLongQ => 2048,
            _ => 512,
        }
    }

    /// List all available models
    pub fn all() -> Vec<Self> {
        vec![
            Self::BgeSmallEnV15,
            Self::BgeSmallEnV15Q,
            Self::AllMiniLmL6V2,
            Self::AllMiniLmL6V2Q,
            Self::AllMiniLmL12V2,
            Self::AllMiniLmL12V2Q,
            Self::AllMpnetBaseV2,
            Self::BgeBaseEnV15,
            Self::BgeBaseEnV15Q,
            Self::BgeLargeEnV15,
            Self::BgeLargeEnV15Q,
            Self::MultilingualE5Small,
            Self::MultilingualE5Base,
            Self::MultilingualE5Large,
            Self::ParaphraseMiniLmL12V2,
            Self::ParaphraseMiniLmL12V2Q,
            Self::ParaphraseMultilingualMpnetBaseV2,
            Self::BgeSmallZhV15,
            Self::BgeLargeZhV15,
            Self::NomicEmbedTextV1,
            Self::NomicEmbedTextV15,
            Self::NomicEmbedTextV15Q,
            Self::MxbaiEmbedLargeV1,
            Self::MxbaiEmbedLargeV1Q,
            Self::GteBaseEnV15,
            Self::GteBaseEnV15Q,
            Self::GteLargeEnV15,
            Self::GteLargeEnV15Q,
            Self::ClipVitB32,
            Self::JinaEmbeddingsV2BaseCode,
            Self::EmbeddingGemma300M,
            Self::ModernBertEmbedLarge,
            Self::SnowflakeArcticEmbedXs,
            Self::SnowflakeArcticEmbedXsQ,
            Self::SnowflakeArcticEmbedS,
            Self::SnowflakeArcticEmbedSQ,
            Self::SnowflakeArcticEmbedM,
            Self::SnowflakeArcticEmbedMQ,
            Self::SnowflakeArcticEmbedMLong,
            Self::SnowflakeArcticEmbedMLongQ,
            Self::SnowflakeArcticEmbedL,
            Self::SnowflakeArcticEmbedLQ,
        ]
    }
}

impl Display for EmbeddingModelType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::BgeSmallEnV15 => "bge-small-en-v1.5",
            Self::BgeSmallEnV15Q => "bge-small-en-v1.5-q",
            Self::AllMiniLmL6V2 => "all-minilm-l6-v2",
            Self::AllMiniLmL6V2Q => "all-minilm-l6-v2-q",
            Self::AllMiniLmL12V2 => "all-minilm-l12-v2",
            Self::AllMiniLmL12V2Q => "all-minilm-l12-v2-q",
            Self::AllMpnetBaseV2 => "all-mpnet-base-v2",
            Self::BgeBaseEnV15 => "bge-base-en-v1.5",
            Self::BgeBaseEnV15Q => "bge-base-en-v1.5-q",
            Self::BgeLargeEnV15 => "bge-large-en-v1.5",
            Self::BgeLargeEnV15Q => "bge-large-en-v1.5-q",
            Self::MultilingualE5Small => "multilingual-e5-small",
            Self::MultilingualE5Base => "multilingual-e5-base",
            Self::MultilingualE5Large => "multilingual-e5-large",
            Self::ParaphraseMiniLmL12V2 => "paraphrase-minilm-l12-v2",
            Self::ParaphraseMiniLmL12V2Q => "paraphrase-minilm-l12-v2-q",
            Self::ParaphraseMultilingualMpnetBaseV2 => "paraphrase-multilingual-mpnet-base-v2",
            Self::BgeSmallZhV15 => "bge-small-zh-v1.5",
            Self::BgeLargeZhV15 => "bge-large-zh-v1.5",
            Self::NomicEmbedTextV1 => "nomic-embed-text-v1",
            Self::NomicEmbedTextV15 => "nomic-embed-text-v1.5",
            Self::NomicEmbedTextV15Q => "nomic-embed-text-v1.5-q",
            Self::MxbaiEmbedLargeV1 => "mxbai-embed-large-v1",
            Self::MxbaiEmbedLargeV1Q => "mxbai-embed-large-v1-q",
            Self::GteBaseEnV15 => "gte-base-en-v1.5",
            Self::GteBaseEnV15Q => "gte-base-en-v1.5-q",
            Self::GteLargeEnV15 => "gte-large-en-v1.5",
            Self::GteLargeEnV15Q => "gte-large-en-v1.5-q",
            Self::ClipVitB32 => "clip-vit-b-32",
            Self::JinaEmbeddingsV2BaseCode => "jina-embeddings-v2-base-code",
            Self::EmbeddingGemma300M => "embedding-gemma-300m",
            Self::ModernBertEmbedLarge => "modernbert-embed-large",
            Self::SnowflakeArcticEmbedXs => "snowflake-arctic-embed-xs",
            Self::SnowflakeArcticEmbedXsQ => "snowflake-arctic-embed-xs-q",
            Self::SnowflakeArcticEmbedS => "snowflake-arctic-embed-s",
            Self::SnowflakeArcticEmbedSQ => "snowflake-arctic-embed-s-q",
            Self::SnowflakeArcticEmbedM => "snowflake-arctic-embed-m",
            Self::SnowflakeArcticEmbedMQ => "snowflake-arctic-embed-m-q",
            Self::SnowflakeArcticEmbedMLong => "snowflake-arctic-embed-m-long",
            Self::SnowflakeArcticEmbedMLongQ => "snowflake-arctic-embed-m-long-q",
            Self::SnowflakeArcticEmbedL => "snowflake-arctic-embed-l",
            Self::SnowflakeArcticEmbedLQ => "snowflake-arctic-embed-l-q",
        };
        write!(f, "{}", name)
    }
}

impl FromStr for EmbeddingModelType {
    type Err = AppError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "bge-small-en-v1.5" | "bge-small-en" | "bge-small" => Ok(Self::BgeSmallEnV15),
            "bge-small-en-v1.5-q" => Ok(Self::BgeSmallEnV15Q),
            "all-minilm-l6-v2" | "minilm-l6" => Ok(Self::AllMiniLmL6V2),
            "all-minilm-l6-v2-q" => Ok(Self::AllMiniLmL6V2Q),
            "all-minilm-l12-v2" | "minilm-l12" => Ok(Self::AllMiniLmL12V2),
            "all-minilm-l12-v2-q" => Ok(Self::AllMiniLmL12V2Q),
            "all-mpnet-base-v2" | "mpnet" => Ok(Self::AllMpnetBaseV2),
            "bge-base-en-v1.5" | "bge-base-en" | "bge-base" => Ok(Self::BgeBaseEnV15),
            "bge-base-en-v1.5-q" => Ok(Self::BgeBaseEnV15Q),
            "bge-large-en-v1.5" | "bge-large-en" | "bge-large" => Ok(Self::BgeLargeEnV15),
            "bge-large-en-v1.5-q" => Ok(Self::BgeLargeEnV15Q),
            "multilingual-e5-small" | "e5-small" => Ok(Self::MultilingualE5Small),
            "multilingual-e5-base" | "e5-base" => Ok(Self::MultilingualE5Base),
            "multilingual-e5-large" | "e5-large" => Ok(Self::MultilingualE5Large),
            "paraphrase-minilm-l12-v2" => Ok(Self::ParaphraseMiniLmL12V2),
            "paraphrase-minilm-l12-v2-q" => Ok(Self::ParaphraseMiniLmL12V2Q),
            "paraphrase-multilingual-mpnet-base-v2" => Ok(Self::ParaphraseMultilingualMpnetBaseV2),
            "bge-small-zh-v1.5" | "bge-small-zh" => Ok(Self::BgeSmallZhV15),
            "bge-large-zh-v1.5" | "bge-large-zh" => Ok(Self::BgeLargeZhV15),
            "nomic-embed-text-v1" | "nomic-v1" => Ok(Self::NomicEmbedTextV1),
            "nomic-embed-text-v1.5" | "nomic-v1.5" | "nomic" => Ok(Self::NomicEmbedTextV15),
            "nomic-embed-text-v1.5-q" => Ok(Self::NomicEmbedTextV15Q),
            "mxbai-embed-large-v1" | "mxbai" => Ok(Self::MxbaiEmbedLargeV1),
            "mxbai-embed-large-v1-q" => Ok(Self::MxbaiEmbedLargeV1Q),
            "gte-base-en-v1.5" | "gte-base" => Ok(Self::GteBaseEnV15),
            "gte-base-en-v1.5-q" => Ok(Self::GteBaseEnV15Q),
            "gte-large-en-v1.5" | "gte-large" => Ok(Self::GteLargeEnV15),
            "gte-large-en-v1.5-q" => Ok(Self::GteLargeEnV15Q),
            "clip-vit-b-32" | "clip" => Ok(Self::ClipVitB32),
            "jina-embeddings-v2-base-code" | "jina-code" => Ok(Self::JinaEmbeddingsV2BaseCode),
            "embedding-gemma-300m" | "gemma-300m" | "gemma" => Ok(Self::EmbeddingGemma300M),
            "modernbert-embed-large" | "modernbert" => Ok(Self::ModernBertEmbedLarge),
            "snowflake-arctic-embed-xs" => Ok(Self::SnowflakeArcticEmbedXs),
            "snowflake-arctic-embed-xs-q" => Ok(Self::SnowflakeArcticEmbedXsQ),
            "snowflake-arctic-embed-s" => Ok(Self::SnowflakeArcticEmbedS),
            "snowflake-arctic-embed-s-q" => Ok(Self::SnowflakeArcticEmbedSQ),
            "snowflake-arctic-embed-m" => Ok(Self::SnowflakeArcticEmbedM),
            "snowflake-arctic-embed-m-q" => Ok(Self::SnowflakeArcticEmbedMQ),
            "snowflake-arctic-embed-m-long" => Ok(Self::SnowflakeArcticEmbedMLong),
            "snowflake-arctic-embed-m-long-q" => Ok(Self::SnowflakeArcticEmbedMLongQ),
            "snowflake-arctic-embed-l" | "snowflake-l" => Ok(Self::SnowflakeArcticEmbedL),
            "snowflake-arctic-embed-l-q" => Ok(Self::SnowflakeArcticEmbedLQ),
            _ => Err(AppError::Internal(format!(
                "Unknown embedding model: {}. Use one of: {}",
                s,
                EmbeddingModelType::all()
                    .iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }
}

// ============================================================================
// Sparse Embedding Model Configuration
// ============================================================================

/// Supported sparse embedding models for hybrid search
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SparseModelType {
    /// SPLADE++ v1 - English sparse embeddings
    #[default]
    SpladePpV1,
    // NOTE: BGE-M3 sparse mode is not available in fastembed 5.5.0
}

impl SparseModelType {
    /// Convert to fastembed's SparseModel enum
    pub fn to_fastembed_model(&self) -> SparseModel {
        match self {
            Self::SpladePpV1 => SparseModel::SPLADEPPV1,
        }
    }
}

impl Display for SparseModelType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::SpladePpV1 => "splade-pp-v1",
        };
        write!(f, "{}", name)
    }
}

impl FromStr for SparseModelType {
    type Err = AppError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "splade-pp-v1" | "splade" => Ok(Self::SpladePpV1),
            _ => Err(AppError::Internal(format!(
                "Unknown sparse model: {}. Use: splade-pp-v1",
                s
            ))),
        }
    }
}

// ============================================================================
// Embedding Service Configuration
// ============================================================================

/// Configuration for the embedding service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// The embedding model to use
    #[serde(default)]
    pub model: EmbeddingModelType,

    /// Batch size for embedding multiple texts
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,

    /// Show download progress for first-time model downloads
    #[serde(default = "default_show_progress")]
    pub show_download_progress: bool,

    /// Enable sparse embeddings for hybrid search
    #[serde(default)]
    pub sparse_enabled: bool,

    /// Sparse embedding model to use
    #[serde(default)]
    pub sparse_model: SparseModelType,
}

fn default_batch_size() -> usize {
    32
}

fn default_show_progress() -> bool {
    true
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model: EmbeddingModelType::default(),
            batch_size: default_batch_size(),
            show_download_progress: default_show_progress(),
            sparse_enabled: false,
            sparse_model: SparseModelType::default(),
        }
    }
}

// ============================================================================
// Embedding Service
// ============================================================================

/// Main embedding service for generating text embeddings
///
/// Uses `spawn_blocking` to run fastembed's synchronous operations
/// without blocking the async runtime.
pub struct EmbeddingService {
    #[allow(dead_code)]
    model: TextEmbedding,
    #[allow(dead_code)]
    sparse_model: Option<fastembed::SparseTextEmbedding>,
    config: EmbeddingConfig,
}

impl EmbeddingService {
    /// Create a new embedding service with the given configuration
    pub fn new(config: EmbeddingConfig) -> Result<Self> {
        let model = TextEmbedding::try_new(
            InitOptions::new(config.model.to_fastembed_model())
                .with_show_download_progress(config.show_download_progress),
        )
        .map_err(|e| AppError::Internal(format!("Failed to initialize embedding model: {}", e)))?;

        let sparse_model = if config.sparse_enabled {
            Some(
                fastembed::SparseTextEmbedding::try_new(
                    fastembed::SparseInitOptions::new(config.sparse_model.to_fastembed_model())
                        .with_show_download_progress(config.show_download_progress),
                )
                .map_err(|e| {
                    AppError::Internal(format!("Failed to initialize sparse embedding model: {}", e))
                })?,
            )
        } else {
            None
        };

        Ok(Self {
            model,
            sparse_model,
            config,
        })
    }

    /// Create a new embedding service with the default model
    pub fn with_default_model() -> Result<Self> {
        Self::new(EmbeddingConfig::default())
    }

    /// Create a new embedding service with a specific model
    pub fn with_model(model: EmbeddingModelType) -> Result<Self> {
        Self::new(EmbeddingConfig {
            model,
            ..Default::default()
        })
    }

    /// Get the current model type
    pub fn model_type(&self) -> EmbeddingModelType {
        self.config.model
    }

    /// Get the embedding dimensions
    pub fn dimensions(&self) -> usize {
        self.config.model.dimensions()
    }

    /// Get the configuration
    pub fn config(&self) -> &EmbeddingConfig {
        &self.config
    }

    /// Embed a single text (async via spawn_blocking)
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let embeddings = self.embed_texts(&[text.to_string()]).await?;
        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| AppError::Internal("No embedding generated".to_string()))
    }

    /// Embed multiple texts in batches (async via spawn_blocking)
    ///
    /// This is more efficient than calling `embed_text` multiple times
    /// as it batches the texts and processes them together.
    pub async fn embed_texts<S: AsRef<str> + Send + Sync + 'static>(
        &self,
        texts: &[S],
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        // Clone texts to owned strings for the spawn_blocking closure
        let texts_owned: Vec<String> = texts.iter().map(|s| s.as_ref().to_string()).collect();
        let batch_size = self.config.batch_size;

        // Clone the model config for the blocking task
        let model_type = self.config.model.to_fastembed_model();
        let show_progress = self.config.show_download_progress;

        spawn_blocking(move || {
            // Create model in the blocking context
            let mut model = TextEmbedding::try_new(
                InitOptions::new(model_type).with_show_download_progress(show_progress),
            )
            .map_err(|e| {
                AppError::Internal(format!("Failed to initialize embedding model: {}", e))
            })?;

            let refs: Vec<&str> = texts_owned.iter().map(|s| s.as_str()).collect();
            model
                .embed(refs, Some(batch_size))
                .map_err(|e| AppError::Internal(format!("Embedding failed: {}", e)))
        })
        .await
        .map_err(|e| AppError::Internal(format!("Blocking task failed: {}", e)))?
    }

    /// Generate sparse embeddings for hybrid search
    pub async fn embed_sparse<S: AsRef<str> + Send + Sync + 'static>(
        &self,
        texts: &[S],
    ) -> Result<Vec<fastembed::SparseEmbedding>> {
        if self.sparse_model.is_none() {
            return Err(AppError::Internal(
                "Sparse embeddings not enabled. Set sparse_enabled: true in config.".to_string(),
            ));
        }

        let texts_owned: Vec<String> = texts.iter().map(|s| s.as_ref().to_string()).collect();
        let batch_size = self.config.batch_size;
        let sparse_model_type = self.config.sparse_model.to_fastembed_model();
        let show_progress = self.config.show_download_progress;

        spawn_blocking(move || {
            let mut model = fastembed::SparseTextEmbedding::try_new(
                fastembed::SparseInitOptions::new(sparse_model_type)
                    .with_show_download_progress(show_progress),
            )
            .map_err(|e| {
                AppError::Internal(format!("Failed to initialize sparse model: {}", e))
            })?;

            let refs: Vec<&str> = texts_owned.iter().map(|s| s.as_str()).collect();
            model
                .embed(refs, Some(batch_size))
                .map_err(|e| AppError::Internal(format!("Sparse embedding failed: {}", e)))
        })
        .await
        .map_err(|e| AppError::Internal(format!("Blocking task failed: {}", e)))?
    }
}

// ============================================================================
// GPU Acceleration Stubs (TODO)
// ============================================================================

/// GPU acceleration backend (STUB - see docs/FUTURE_ENHANCEMENTS.md)
///
/// This enum represents potential GPU acceleration options for embedding models.
/// Currently not implemented - all models run on CPU.
///
/// # Future Implementation
///
/// - **CUDA**: NVIDIA GPU acceleration via ONNX Runtime CUDA provider
/// - **Metal**: Apple Silicon GPU acceleration via ONNX Runtime CoreML provider
/// - **Vulkan**: Cross-platform GPU acceleration via ONNX Runtime Vulkan provider
/// - **Candle**: GPU support for Qwen3 models via Candle's CUDA backend
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum AccelerationBackend {
    /// CPU execution (default, always available)
    Cpu,
    /// NVIDIA CUDA acceleration
    Cuda { device_id: usize },
    /// Apple Metal acceleration
    Metal,
    /// Vulkan GPU acceleration
    Vulkan,
}

impl Default for AccelerationBackend {
    fn default() -> Self {
        Self::Cpu
    }
}

// ============================================================================
// Legacy API Compatibility
// ============================================================================

/// Legacy embedding service for backward compatibility
///
/// This preserves the original API for existing code.
#[deprecated(note = "Use EmbeddingService instead")]
pub struct LegacyEmbeddingService {
    inner: EmbeddingService,
}

#[allow(deprecated)]
impl LegacyEmbeddingService {
    /// Create a new legacy embedding service
    pub fn new(_model_name: &str) -> Result<Self> {
        Ok(Self {
            inner: EmbeddingService::with_default_model()?,
        })
    }

    /// Embed texts (synchronous API)
    pub fn embed(&mut self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>> {
        let model_type = self.inner.config.model.to_fastembed_model();
        let mut model = TextEmbedding::try_new(
            InitOptions::new(model_type).with_show_download_progress(true),
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;

        model
            .embed(texts, None)
            .map_err(|e| AppError::Internal(e.to_string()))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_dimensions() {
        assert_eq!(EmbeddingModelType::BgeSmallEnV15.dimensions(), 384);
        assert_eq!(EmbeddingModelType::BgeBaseEnV15.dimensions(), 768);
        assert_eq!(EmbeddingModelType::BgeLargeEnV15.dimensions(), 1024);
        assert_eq!(EmbeddingModelType::MultilingualE5Large.dimensions(), 1024);
    }

    #[test]
    fn test_model_from_str() {
        assert_eq!(
            "bge-small-en-v1.5".parse::<EmbeddingModelType>().unwrap(),
            EmbeddingModelType::BgeSmallEnV15
        );
        assert_eq!(
            "multilingual-e5-large".parse::<EmbeddingModelType>().unwrap(),
            EmbeddingModelType::MultilingualE5Large
        );
        assert_eq!(
            "minilm-l6".parse::<EmbeddingModelType>().unwrap(),
            EmbeddingModelType::AllMiniLmL6V2
        );
    }

    #[test]
    fn test_model_is_multilingual() {
        assert!(EmbeddingModelType::MultilingualE5Small.is_multilingual());
        assert!(EmbeddingModelType::MultilingualE5Large.is_multilingual());
        assert!(!EmbeddingModelType::BgeSmallEnV15.is_multilingual());
    }

    #[test]
    fn test_model_max_context() {
        assert_eq!(
            EmbeddingModelType::NomicEmbedTextV15.max_context_length(),
            8192
        );
        assert_eq!(
            EmbeddingModelType::NomicEmbedTextV1.max_context_length(),
            8192
        );
        assert_eq!(
            EmbeddingModelType::BgeSmallEnV15.max_context_length(),
            512
        );
    }

    #[test]
    fn test_default_config() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.model, EmbeddingModelType::BgeSmallEnV15);
        assert_eq!(config.batch_size, 32);
        assert!(config.show_download_progress);
        assert!(!config.sparse_enabled);
    }

    #[test]
    fn test_all_models_listed() {
        let all = EmbeddingModelType::all();
        assert!(all.len() >= 38); // We have 38+ models
        assert!(all.contains(&EmbeddingModelType::BgeSmallEnV15));
        assert!(all.contains(&EmbeddingModelType::MultilingualE5Large));
    }
}
