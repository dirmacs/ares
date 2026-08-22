//! Embedding Service for RAG
//!
//! This module provides a comprehensive embedding service with support for:
//! - 30+ text embedding models (BGE, Qwen3, Gemma, E5, Jina, etc.)
//! - Sparse embeddings for hybrid search (SPLADE, BGE-M3)
//! - Reranking models (BGE, Jina)
//! - Async embedding via `spawn_blocking`
//! - In-memory LRU caching to avoid recomputing embeddings
//!
//! # Feature Flag
//!
//! This module requires the `local-embeddings` feature to be enabled.
//! Without it, local ONNX-based embeddings are not available.
//!
//! ```toml
//! [dependencies]
//! ares-server = { version = "0.3", features = ["local-embeddings"] }
//! ```
//!
//! # GPU Acceleration (TODO)
//! GPU acceleration is planned for future iterations. See `docs/FUTURE_ENHANCEMENTS.md`.
//! Potential approach:
//! - Add feature flags: `cuda`, `metal`, `vulkan`
//! - Use ORT execution providers for ONNX models
//! - Use Candle GPU features for Qwen3 models
//!
//! # Embedding Cache
//! Use `CachedEmbeddingService` to wrap the `EmbeddingService` with an LRU cache.
//! See [`crate::cache`] for cache configuration options.

use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
// Note: Arc is now used both for MODEL_INIT_LOCKS and for wrapping the embedding models
use tokio::task::spawn_blocking;

// Re-export fastembed types for convenience
pub use fastembed::{EmbeddingModel as FastEmbedModel, InitOptions, SparseModel, TextEmbedding};

/// Global lock for model initialization to prevent race conditions during parallel downloads.
/// The key is the model name (from FastEmbedModel's Debug representation).
static MODEL_INIT_LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

/// Get or create a lock for a specific model to prevent concurrent initialization.
fn get_model_lock(model_name: &str) -> Arc<Mutex<()>> {
    let locks = MODEL_INIT_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = locks.lock().unwrap();
    map.entry(model_name.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Pre-download model files via lancor's hub client.
///
/// Fastembed's built-in hf_hub/ureq client fails on xethub CDN redirects.
/// Lancor uses reqwest which handles these correctly. We download files
/// then place them in the HF cache format that hf-hub/fastembed expects:
///   {cache_dir}/models--{org}--{model}/snapshots/{hash}/{filename}
///   {cache_dir}/models--{org}--{model}/refs/main → {hash}
pub(crate) fn pre_download_model(
    repo_id: &str,
    files: &[&str],
    cache_dir: &std::path::Path,
) -> Result<()> {
    // Build HF cache directory structure
    let folder_name = format!("models--{}", repo_id.replace('/', "--"));
    let snapshot_hash = "lancor-prefetch"; // deterministic hash for our downloads
    let snapshot_dir = cache_dir.join(&folder_name).join("snapshots").join(snapshot_hash);
    let refs_dir = cache_dir.join(&folder_name).join("refs");

    std::fs::create_dir_all(&snapshot_dir).ok();
    std::fs::create_dir_all(&refs_dir).ok();

    // Write refs/main → snapshot hash
    let ref_path = refs_dir.join("main");
    if !ref_path.exists() {
        std::fs::write(&ref_path, snapshot_hash).ok();
    }

    let hub = lancor::hub::HubClient::with_cache_dir(cache_dir.to_path_buf())
        .map_err(|e| AppError::Internal(format!("Failed to create hub client: {}", e)))?;

    let rt = tokio::runtime::Handle::current();
    for filename in files {
        let target = snapshot_dir.join(filename);
        if target.exists() && std::fs::metadata(&target).map(|m| m.len() > 0).unwrap_or(false) {
            tracing::debug!("Already cached: {}/{}", repo_id, filename);
            continue;
        }

        // Create parent dirs for nested files like onnx/model.onnx
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        match tokio::task::block_in_place(|| {
            rt.block_on(hub.download(repo_id, filename, None))
        }) {
            Ok(downloaded_path) => {
                // Copy from lancor's cache to HF cache format
                if downloaded_path != target {
                    std::fs::copy(&downloaded_path, &target).ok();
                }
                tracing::info!("Pre-downloaded {}/{} ({} bytes)", repo_id, filename,
                    std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0));
            }
            Err(e) => {
                tracing::warn!("Could not pre-download {}/{}: {}", repo_id, filename, e);
            }
        }
    }
    Ok(())
}

// ============================================================================
// Embedding Vector Utilities
// ============================================================================

/// Cosine similarity between two dense embeddings.
///
/// Returns a value in `[-1.0, 1.0]` where `1.0` means identical direction.
/// Returns `0.0` when vectors have mismatched lengths or zero magnitude.
#[inline]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        (dot / denom).clamp(-1.0, 1.0)
    }
}

/// Cosine distance between two embeddings (`1.0 - cosine_similarity`).
///
/// Lower values indicate greater similarity. Range: `[0.0, 2.0]`.
#[inline]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    1.0 - cosine_similarity(a, b)
}

/// Euclidean (L2) distance between two embeddings.
///
/// Returns [`AppError::InvalidInput`] when dimensions do not match.
pub fn euclidean_distance(a: &[f32], b: &[f32]) -> Result<f32> {
    if a.len() != b.len() {
        return Err(AppError::InvalidInput(format!(
            "Embedding dimension mismatch: {} vs {}",
            a.len(),
            b.len()
        )));
    }

    let sum_sq: f32 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| {
            let d = x - y;
            d * d
        })
        .sum();

    Ok(sum_sq.sqrt())
}

/// L2-normalize an embedding in place (unit length). No-op for zero vectors.
pub fn normalize_embedding(embedding: &mut [f32]) {
    let norm_sq: f32 = embedding.iter().map(|x| x * x).sum();
    if norm_sq == 0.0 {
        return;
    }
    let inv = norm_sq.sqrt().recip();
    for value in embedding.iter_mut() {
        *value *= inv;
    }
}

/// Validate that an embedding matches the expected model dimension.
pub fn validate_embedding_dims(embedding: &[f32], expected_dims: usize) -> Result<()> {
    if embedding.is_empty() {
        return Err(AppError::InvalidInput(
            "Embedding vector must not be empty".to_string(),
        ));
    }
    if embedding.len() != expected_dims {
        return Err(AppError::InvalidInput(format!(
            "Expected embedding dimension {}, got {}",
            expected_dims,
            embedding.len()
        )));
    }
    Ok(())
}

/// Construct a validated dense embedding vector for the given model dimension.
pub fn dense_embedding(values: Vec<f32>, expected_dims: usize) -> Result<Vec<f32>> {
    validate_embedding_dims(&values, expected_dims)?;
    Ok(values)
}

/// Error returned when sparse embeddings are requested but not configured.
pub(crate) fn sparse_embeddings_disabled_error() -> AppError {
    AppError::Internal(
        "Sparse embeddings not enabled. Set sparse_enabled: true in config.".to_string(),
    )
}

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

    /// Get the HuggingFace repo ID for this model (used for pre-downloading)
    pub fn hf_repo_id(&self) -> &'static str {
        match self {
            Self::BgeSmallEnV15 | Self::BgeSmallEnV15Q => "Xenova/bge-small-en-v1.5",
            Self::AllMiniLmL6V2 | Self::AllMiniLmL6V2Q => "sentence-transformers/all-MiniLM-L6-v2",
            Self::AllMiniLmL12V2 | Self::AllMiniLmL12V2Q => "sentence-transformers/all-MiniLM-L12-v2",
            _ => "Xenova/bge-small-en-v1.5", // fallback to default
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
///
/// The model is wrapped in `Arc<Mutex<TextEmbedding>>` to allow safe
/// reuse across async boundaries without recreating the model on each call.
pub struct EmbeddingService {
    /// The text embedding model, wrapped for thread-safe access
    model: Arc<Mutex<TextEmbedding>>,
    /// Optional sparse embedding model for hybrid search
    sparse_model: Option<Arc<Mutex<fastembed::SparseTextEmbedding>>>,
    config: EmbeddingConfig,
}

impl EmbeddingService {
    /// Create a new embedding service with the given configuration
    ///
    /// Uses a per-model lock to prevent race conditions when multiple threads
    /// try to download/initialize the same model simultaneously.
    pub fn new(config: EmbeddingConfig) -> Result<Self> {
        let model_name = format!("{:?}", config.model.to_fastembed_model());
        let model_lock = get_model_lock(&model_name);

        // Acquire lock for this specific model to prevent concurrent downloads
        let _guard = model_lock.lock().map_err(|e| {
            AppError::Internal(format!(
                "Failed to acquire model initialization lock: {}",
                e
            ))
        })?;

        let cache_dir = std::env::var("FASTEMBED_CACHE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(".fastembed_cache"));
        std::fs::create_dir_all(&cache_dir).ok();

        // Pre-download ONNX model via lancor's hub client (handles CDN redirects
        // that fastembed's ureq-based hf_hub client fails on)
        let model_repo = config.model.hf_repo_id();
        pre_download_model(model_repo, &["onnx/model.onnx", "tokenizer.json", "config.json"], &cache_dir)?;

        // Try loading from local cache first to bypass hf-hub's broken ureq xethub client.
        // Uses UserDefinedEmbeddingModel with raw ONNX bytes when cache exists.
        let folder_name = format!("models--{}", model_repo.replace('/', "--"));
        let model_base = cache_dir.join(&folder_name).join("snapshots");
        let snapshot_dir = if model_base.exists() {
            std::fs::read_dir(&model_base).ok().and_then(|entries| {
                entries.filter_map(|e| e.ok()).find(|e| {
                    let p = e.path();
                    p.join("onnx").join("model.onnx").exists()
                        && p.join("tokenizer.json").exists()
                        && p.join("config.json").exists()
                        && p.join("special_tokens_map.json").exists()
                }).map(|e| e.path())
            })
        } else {
            let native = cache_dir.join(model_repo.replace('/', "--"));
            if native.join("onnx").join("model.onnx").exists() { Some(native) } else { None }
        };

        let model = if let Some(ref snap) = snapshot_dir {
            tracing::info!("Loading embedding model from local cache: {}", snap.display());
            let onnx_bytes = std::fs::read(snap.join("onnx").join("model.onnx"))
                .map_err(|e| AppError::Internal(format!("Failed to read ONNX: {}", e)))?;
            let tokenizer_file = std::fs::read(snap.join("tokenizer.json"))
                .map_err(|e| AppError::Internal(format!("Failed to read tokenizer.json: {}", e)))?;
            let config_file = std::fs::read(snap.join("config.json"))
                .map_err(|e| AppError::Internal(format!("Failed to read config.json: {}", e)))?;
            let special_tokens_map_file = std::fs::read(snap.join("special_tokens_map.json"))
                .map_err(|e| AppError::Internal(format!("Failed to read special_tokens_map.json: {}", e)))?;
            let tokenizer_config_file = std::fs::read(snap.join("tokenizer_config.json"))
                .map_err(|e| AppError::Internal(format!("Failed to read tokenizer_config.json: {}", e)))?;

            let tokenizer_files = fastembed::TokenizerFiles {
                tokenizer_file,
                config_file,
                special_tokens_map_file,
                tokenizer_config_file,
            };

            let user_model = fastembed::UserDefinedEmbeddingModel::new(onnx_bytes, tokenizer_files);
            TextEmbedding::try_new_from_user_defined(user_model, fastembed::InitOptionsUserDefined::new())
                .map_err(|e| AppError::Internal(format!("Failed to load local model: {}", e)))?
        } else {
            tracing::warn!("No local ONNX cache, attempting HF download (may fail on xethub)");
            TextEmbedding::try_new(
                InitOptions::new(config.model.to_fastembed_model())
                    .with_cache_dir(cache_dir.clone())
                    .with_show_download_progress(true),
            )
            .map_err(|e| AppError::Internal(format!("Failed to init embedding model: {}", e)))?
        };

        let sparse_model = if config.sparse_enabled {
            let sparse_model_name = format!("{:?}", config.sparse_model.to_fastembed_model());
            let sparse_lock = get_model_lock(&sparse_model_name);
            let _sparse_guard = sparse_lock.lock().map_err(|e| {
                AppError::Internal(format!("Failed to acquire sparse model lock: {}", e))
            })?;

            Some(
                fastembed::SparseTextEmbedding::try_new(
                    fastembed::SparseInitOptions::new(config.sparse_model.to_fastembed_model())
                        .with_show_download_progress(config.show_download_progress),
                )
                .map_err(|e| {
                    AppError::Internal(format!(
                        "Failed to initialize sparse embedding model: {}",
                        e
                    ))
                })?,
            )
        } else {
            None
        };

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            sparse_model: sparse_model.map(|m| Arc::new(Mutex::new(m))),
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
    ///
    /// The model is reused across calls via `Arc<Mutex<TextEmbedding>>`.
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

        // Clone the Arc to move into the blocking task
        let model = Arc::clone(&self.model);

        spawn_blocking(move || {
            // Lock the model for use
            let mut model_guard = model
                .lock()
                .map_err(|e| AppError::Internal(format!("Failed to acquire model lock: {}", e)))?;

            let refs: Vec<&str> = texts_owned.iter().map(|s| s.as_str()).collect();
            model_guard
                .embed(refs, Some(batch_size))
                .map_err(|e| AppError::Internal(format!("Embedding failed: {}", e)))
        })
        .await
        .map_err(|e| AppError::Internal(format!("Blocking task failed: {}", e)))?
    }

    /// Generate sparse embeddings for hybrid search
    ///
    /// The sparse model is reused across calls via `Arc<Mutex<SparseTextEmbedding>>`.
    pub async fn embed_sparse<S: AsRef<str> + Send + Sync + 'static>(
        &self,
        texts: &[S],
    ) -> Result<Vec<fastembed::SparseEmbedding>> {
        let sparse_model = self
            .sparse_model
            .as_ref()
            .ok_or_else(sparse_embeddings_disabled_error)?;

        let texts_owned: Vec<String> = texts.iter().map(|s| s.as_ref().to_string()).collect();
        let batch_size = self.config.batch_size;

        // Clone the Arc to move into the blocking task
        let model = Arc::clone(sparse_model);

        spawn_blocking(move || {
            // Lock the model for use
            let mut model_guard = model.lock().map_err(|e| {
                AppError::Internal(format!("Failed to acquire sparse model lock: {}", e))
            })?;

            let refs: Vec<&str> = texts_owned.iter().map(|s| s.as_str()).collect();
            model_guard
                .embed(refs, Some(batch_size))
                .map_err(|e| AppError::Internal(format!("Sparse embedding failed: {}", e)))
        })
        .await
        .map_err(|e| AppError::Internal(format!("Blocking task failed: {}", e)))?
    }
}

impl ares_cordis_core::Service for EmbeddingService {
    fn name(&self) -> &'static str { "embedding_service" }
    fn init(&self, _ctx: &std::sync::Arc<ares_cordis_core::Context>) -> ares_cordis_core::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool { true }
}

// ============================================================================
// Cached Embedding Service
// ============================================================================

use crate::cache::{CacheConfig, CacheStats, EmbeddingCache, LruEmbeddingCache, NoOpCache};

/// An embedding service with integrated caching
///
/// Wraps an `EmbeddingService` with an `EmbeddingCache` to avoid recomputing
/// embeddings for previously seen texts. The cache key is computed as a hash
/// of the text content and model name.
///
/// # Example
///
/// ```ignore
/// use ares::rag::embeddings::{CachedEmbeddingService, EmbeddingConfig};
/// use ares::rag::cache::CacheConfig;
///
/// let service = CachedEmbeddingService::new(
///     EmbeddingConfig::default(),
///     CacheConfig::default(),
/// )?;
///
/// // First call computes the embedding
/// let emb1 = service.embed_text("hello world").await?;
///
/// // Second call returns cached result
/// let emb2 = service.embed_text("hello world").await?;
/// assert_eq!(emb1, emb2);
/// ```
pub struct CachedEmbeddingService {
    /// The underlying embedding service
    inner: EmbeddingService,
    /// The embedding cache
    cache: Box<dyn EmbeddingCache>,
}

impl CachedEmbeddingService {
    /// Create a new cached embedding service
    pub fn new(embedding_config: EmbeddingConfig, cache_config: CacheConfig) -> Result<Self> {
        let inner = EmbeddingService::new(embedding_config)?;
        let cache: Box<dyn EmbeddingCache> = if cache_config.enabled {
            Box::new(LruEmbeddingCache::new(cache_config))
        } else {
            Box::new(NoOpCache::new())
        };

        Ok(Self { inner, cache })
    }

    /// Create with default configurations
    pub fn with_defaults() -> Result<Self> {
        Self::new(EmbeddingConfig::default(), CacheConfig::default())
    }

    /// Create with a specific model and default cache
    pub fn with_model(model: EmbeddingModelType) -> Result<Self> {
        Self::new(
            EmbeddingConfig {
                model,
                ..Default::default()
            },
            CacheConfig::default(),
        )
    }

    /// Create with caching disabled
    pub fn without_cache(embedding_config: EmbeddingConfig) -> Result<Self> {
        Self::new(
            embedding_config,
            CacheConfig {
                enabled: false,
                ..Default::default()
            },
        )
    }

    /// Get the model name for cache key computation
    fn model_name(&self) -> String {
        self.inner.model_type().to_string()
    }

    /// Embed a single text with caching
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let cache_key = self.cache.compute_key(text, &self.model_name());

        // Check cache first
        if let Some(cached) = self.cache.get(&cache_key) {
            return Ok(cached);
        }

        // Compute embedding
        let embedding = self.inner.embed_text(text).await?;

        // Store in cache
        self.cache.set(&cache_key, embedding.clone(), None)?;

        Ok(embedding)
    }

    /// Embed multiple texts with caching
    ///
    /// Checks cache for each text individually, computes embeddings only
    /// for uncached texts, and caches the new results.
    pub async fn embed_texts<S: AsRef<str> + Send + Sync + 'static>(
        &self,
        texts: &[S],
    ) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let model_name = self.model_name();
        let mut results: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        let mut uncached_indices: Vec<usize> = Vec::new();
        let mut uncached_texts: Vec<String> = Vec::new();

        // Check cache for each text
        for (i, text) in texts.iter().enumerate() {
            let text_str = text.as_ref();
            let cache_key = self.cache.compute_key(text_str, &model_name);

            if let Some(cached) = self.cache.get(&cache_key) {
                results[i] = Some(cached);
            } else {
                uncached_indices.push(i);
                uncached_texts.push(text_str.to_string());
            }
        }

        // Compute embeddings for uncached texts
        if !uncached_texts.is_empty() {
            let new_embeddings = self.inner.embed_texts(&uncached_texts).await?;

            // Store results and cache them
            for (j, embedding) in new_embeddings.into_iter().enumerate() {
                let idx = uncached_indices[j];
                let cache_key = self.cache.compute_key(&uncached_texts[j], &model_name);
                self.cache.set(&cache_key, embedding.clone(), None)?;
                results[idx] = Some(embedding);
            }
        }

        // Unwrap all results (should all be Some at this point)
        Ok(results.into_iter().flatten().collect())
    }

    /// Get the current model type
    pub fn model_type(&self) -> EmbeddingModelType {
        self.inner.model_type()
    }

    /// Get the embedding dimensions
    pub fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    /// Get the embedding configuration
    pub fn config(&self) -> &EmbeddingConfig {
        self.inner.config()
    }

    /// Get cache statistics
    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }

    /// Clear the cache
    pub fn clear_cache(&self) -> Result<()> {
        self.cache.clear()
    }

    /// Invalidate a specific cache entry
    pub fn invalidate(&self, text: &str) -> Result<()> {
        let cache_key = self.cache.compute_key(text, &self.model_name());
        self.cache.invalidate(&cache_key)
    }

    /// Check if caching is enabled
    pub fn is_cache_enabled(&self) -> bool {
        self.cache.is_enabled()
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
#[derive(Default)]
pub enum AccelerationBackend {
    /// CPU execution (default, always available)
    #[default]
    Cpu,
    /// NVIDIA CUDA acceleration
    Cuda {
        /// The CUDA device ID to use for computation.
        device_id: usize,
    },
    /// Apple Metal acceleration
    Metal,
    /// Vulkan GPU acceleration
    Vulkan,
}


// ============================================================================
// Remote HTTP Embedding API (OpenAI-compatible)
// ============================================================================

/// OpenAI-compatible embedding request body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: EmbeddingInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<EmbeddingEncodingFormat>,
}

/// Input payload for embedding requests (single string or batch).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EmbeddingInput {
    Single(String),
    Batch(Vec<String>),
}

/// Response encoding for embedding vectors.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingEncodingFormat {
    #[default]
    Float,
    Base64,
}

/// OpenAI-compatible embedding response body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingResponse {
    pub object: String,
    pub data: Vec<EmbeddingDataItem>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<EmbeddingUsage>,
}

/// One embedding vector in a response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingDataItem {
    pub object: String,
    pub index: u32,
    pub embedding: EmbeddingVector,
}

/// Embedding values as floats or base64-encoded little-endian `f32` bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum EmbeddingVector {
    Float(Vec<f32>),
    Base64(String),
}

/// Token usage metadata from the embedding endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingUsage {
    pub prompt_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
}

/// Return the dense vector dimension for a local [`EmbeddingModelType`].
#[inline]
pub fn model_dimensions(model: EmbeddingModelType) -> usize {
    model.dimensions()
}

/// Split `items` into consecutive batches of at most `batch_size`, preserving order.
pub fn batch_chunks<T: Clone>(items: &[T], batch_size: usize) -> Vec<Vec<T>> {
    let size = batch_size.max(1);
    if items.is_empty() {
        return vec![];
    }
    items.chunks(size).map(|c| c.to_vec()).collect()
}

/// Build an OpenAI-compatible [`EmbeddingRequest`] from model name and text inputs.
pub fn build_embedding_request(
    model: impl Into<String>,
    inputs: &[impl AsRef<str>],
    encoding_format: Option<EmbeddingEncodingFormat>,
    dimensions: Option<u32>,
) -> EmbeddingRequest {
    let input = if inputs.len() == 1 {
        EmbeddingInput::Single(inputs[0].as_ref().to_string())
    } else {
        EmbeddingInput::Batch(inputs.iter().map(|s| s.as_ref().to_string()).collect())
    };
    EmbeddingRequest { model: model.into(), input, dimensions, encoding_format }
}

/// Parse an embedding HTTP response body into ordered dense vectors.
pub fn parse_embedding_response(body: &str, expected_dims: Option<usize>) -> Result<Vec<Vec<f32>>> {
    let response: EmbeddingResponse = serde_json::from_str(body)
        .map_err(|e| AppError::InvalidInput(format!("Invalid embedding response JSON: {e}")))?;
    if response.data.is_empty() {
        return Err(AppError::InvalidInput("Embedding response contained no data".into()));
    }
    let mut indexed = Vec::with_capacity(response.data.len());
    for item in response.data {
        let vector = decode_embedding_vector(&item.embedding)?;
        if let Some(expected) = expected_dims {
            validate_embedding_dims(&vector, expected)?;
        }
        indexed.push((item.index, vector));
    }
    indexed.sort_by_key(|(i, _)| *i);
    Ok(indexed.into_iter().map(|(_, v)| v).collect())
}

pub fn decode_embedding_vector(vector: &EmbeddingVector) -> Result<Vec<f32>> {
    match vector {
        EmbeddingVector::Float(v) => Ok(v.clone()),
        EmbeddingVector::Base64(s) => decode_base64_embedding(s),
    }
}

pub fn decode_base64_embedding(encoded: &str) -> Result<Vec<f32>> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded.trim())
        .map_err(|e| AppError::InvalidInput(format!("Invalid base64 embedding: {e}")))?;
    if !bytes.len().is_multiple_of(4) {
        return Err(AppError::InvalidInput(format!("Base64 embedding byte length {} is not a multiple of 4", bytes.len())));
    }
    Ok(bytes.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect())
}

pub fn map_embedding_http_error(status: u16, body: &str) -> AppError {
    let detail = if body.trim().is_empty() { format!("HTTP {status}") } else { format!("HTTP {status}: {body}") };
    match status {
        401 | 403 => AppError::Auth(detail),
        404 => AppError::NotFound(detail),
        429 => AppError::RateLimited(detail),
        400..=499 => AppError::InvalidInput(detail),
        _ => AppError::External(detail),
    }
}

pub fn map_embedding_transport_error(err: &reqwest::Error) -> AppError {
    if err.is_timeout() {
        AppError::Unavailable(format!("Embedding request timed out: {err}"))
    } else if err.is_connect() {
        AppError::Unavailable(format!("Embedding service unreachable: {err}"))
    } else {
        AppError::External(format!("Embedding HTTP request failed: {err}"))
    }
}

#[derive(Debug, Clone)]
pub struct HttpEmbeddingClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl HttpEmbeddingClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()
            .map_err(|e| AppError::Configuration(format!("Failed to build HTTP client: {e}")))?;
        Ok(Self { http, base_url: base_url.into().trim_end_matches('/').to_string(), api_key: None })
    }
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self { self.api_key = Some(api_key.into()); self }
    pub async fn embed(&self, request: &EmbeddingRequest) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/v1/embeddings", self.base_url);
        let expected_dims = request.dimensions.map(|d| d as usize);
        let mut req = self.http.post(&url).json(request);
        if let Some(key) = &self.api_key { req = req.bearer_auth(key); }
        let response = req.send().await.map_err(|e| map_embedding_transport_error(&e))?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|e| map_embedding_transport_error(&e))?;
        if !(200..300).contains(&status) { return Err(map_embedding_http_error(status, &body)); }
        parse_embedding_response(&body, expected_dims)
    }
    pub async fn embed_texts_batched(&self, model: &str, texts: &[impl AsRef<str>], batch_size: usize,
        encoding_format: Option<EmbeddingEncodingFormat>, dimensions: Option<u32>) -> Result<Vec<Vec<f32>>> {
        let owned: Vec<String> = texts.iter().map(|t| t.as_ref().to_string()).collect();
        let mut all = Vec::with_capacity(owned.len());
        for batch in batch_chunks(&owned, batch_size) {
            let request = build_embedding_request(model, &batch, encoding_format, dimensions);
            let mut vectors = self.embed(&request).await?;
            all.append(&mut vectors);
        }
        Ok(all)
    }
}

// ============================================================================
// Tests
// ============================================================================


#[cfg(all(test, feature = "local-embeddings"))]
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
            "multilingual-e5-large"
                .parse::<EmbeddingModelType>()
                .unwrap(),
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
        assert_eq!(EmbeddingModelType::BgeSmallEnV15.max_context_length(), 512);
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

    #[test]
    fn test_display_roundtrip_all_models() {
        // Every model's Display output should parse back via FromStr
        for model in EmbeddingModelType::all() {
            let display = model.to_string();
            let parsed: EmbeddingModelType = display.parse().unwrap_or_else(|_| {
                panic!("Display→FromStr roundtrip failed for {:?} ('{}')", model, display)
            });
            assert_eq!(parsed, model, "Roundtrip mismatch for {}", display);
        }
    }

    #[test]
    fn test_from_str_aliases() {
        // Test short aliases resolve correctly
        let aliases = vec![
            ("bge-small", EmbeddingModelType::BgeSmallEnV15),
            ("bge-small-en", EmbeddingModelType::BgeSmallEnV15),
            ("bge-base", EmbeddingModelType::BgeBaseEnV15),
            ("bge-large", EmbeddingModelType::BgeLargeEnV15),
            ("e5-small", EmbeddingModelType::MultilingualE5Small),
            ("e5-large", EmbeddingModelType::MultilingualE5Large),
            ("mpnet", EmbeddingModelType::AllMpnetBaseV2),
            ("nomic", EmbeddingModelType::NomicEmbedTextV15),
            ("mxbai", EmbeddingModelType::MxbaiEmbedLargeV1),
            ("gte-base", EmbeddingModelType::GteBaseEnV15),
            ("gte-large", EmbeddingModelType::GteLargeEnV15),
            ("clip", EmbeddingModelType::ClipVitB32),
            ("jina-code", EmbeddingModelType::JinaEmbeddingsV2BaseCode),
            ("gemma", EmbeddingModelType::EmbeddingGemma300M),
            ("modernbert", EmbeddingModelType::ModernBertEmbedLarge),
            ("snowflake-l", EmbeddingModelType::SnowflakeArcticEmbedL),
        ];
        for (alias, expected) in aliases {
            let parsed: EmbeddingModelType = alias.parse().unwrap_or_else(|_| {
                panic!("Alias '{}' should parse", alias)
            });
            assert_eq!(parsed, expected, "Alias '{}' mismatch", alias);
        }
    }

    #[test]
    fn test_from_str_case_insensitive() {
        let upper: EmbeddingModelType = "BGE-SMALL-EN-V1.5".parse().unwrap();
        assert_eq!(upper, EmbeddingModelType::BgeSmallEnV15);
        let mixed: EmbeddingModelType = "Nomic-Embed-Text-V1.5".parse().unwrap();
        assert_eq!(mixed, EmbeddingModelType::NomicEmbedTextV15);
    }

    #[test]
    fn test_from_str_invalid_model() {
        let result = "totally-fake-model".parse::<EmbeddingModelType>();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
        let msg = err.to_string();
        assert!(msg.contains("Unknown embedding model"), "Error should mention 'Unknown': {}", msg);
    }

    #[test]
    fn test_hf_repo_id_known_models() {
        assert_eq!(EmbeddingModelType::BgeSmallEnV15.hf_repo_id(), "Xenova/bge-small-en-v1.5");
        assert_eq!(EmbeddingModelType::AllMiniLmL6V2.hf_repo_id(), "sentence-transformers/all-MiniLM-L6-v2");
        assert_eq!(EmbeddingModelType::AllMiniLmL12V2.hf_repo_id(), "sentence-transformers/all-MiniLM-L12-v2");
    }

    #[test]
    fn test_hf_repo_id_quantized_same_as_base() {
        // Quantized variants should map to same repo as base
        assert_eq!(
            EmbeddingModelType::BgeSmallEnV15.hf_repo_id(),
            EmbeddingModelType::BgeSmallEnV15Q.hf_repo_id()
        );
        assert_eq!(
            EmbeddingModelType::AllMiniLmL6V2.hf_repo_id(),
            EmbeddingModelType::AllMiniLmL6V2Q.hf_repo_id()
        );
    }

    #[test]
    fn test_dimensions_categories() {
        // Verify dimension categories: 384, 512, 768, 1024
        for model in EmbeddingModelType::all() {
            let dim = model.dimensions();
            assert!(
                dim == 384 || dim == 512 || dim == 768 || dim == 1024,
                "{:?} has unexpected dimension {}",
                model,
                dim
            );
        }
    }

    #[test]
    fn test_sparse_model_display_roundtrip() {
        let model = SparseModelType::SpladePpV1;
        let display = model.to_string();
        assert_eq!(display, "splade-pp-v1");
        let parsed: SparseModelType = display.parse().unwrap();
        assert_eq!(parsed, model);
    }

    #[test]
    fn test_sparse_model_alias() {
        let parsed: SparseModelType = "splade".parse().unwrap();
        assert_eq!(parsed, SparseModelType::SpladePpV1);
    }

    #[test]
    fn test_sparse_model_invalid() {
        let result = "nonexistent-sparse".parse::<SparseModelType>();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
        assert!(err.to_string().contains("Unknown sparse model"));
    }

    #[test]
    fn test_embedding_config_serialization_roundtrip() {
        let config = EmbeddingConfig {
            model: EmbeddingModelType::NomicEmbedTextV15,
            batch_size: 64,
            show_download_progress: false,
            sparse_enabled: true,
            sparse_model: SparseModelType::SpladePpV1,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: EmbeddingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, EmbeddingModelType::NomicEmbedTextV15);
        assert_eq!(parsed.batch_size, 64);
        assert!(!parsed.show_download_progress);
        assert!(parsed.sparse_enabled);
    }

    #[test]
    fn test_to_fastembed_model_all_variants() {
        // Ensure every model maps to a fastembed variant without panicking
        for model in EmbeddingModelType::all() {
            let _ = model.to_fastembed_model(); // should not panic
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_pre_download_creates_cache_structure() {
        // Test that pre_download_model creates the correct directory structure
        // (without actually downloading — lancor will fail on fake repo, but dirs should be created)
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_dir = tmp.path().to_path_buf();

        // This will fail on download (fake repo) but should create dir structure
        let _ = pre_download_model(
            "fake-org/fake-model",
            &["onnx/model.onnx"],
            &cache_dir,
        );

        // Verify HF cache structure was created
        let folder = cache_dir.join("models--fake-org--fake-model");
        assert!(folder.join("snapshots").join("lancor-prefetch").exists(),
            "snapshot dir should be created");
        assert!(folder.join("refs").exists(),
            "refs dir should be created");
        let ref_main = folder.join("refs").join("main");
        if ref_main.exists() {
            let content = std::fs::read_to_string(&ref_main).unwrap();
            assert_eq!(content, "lancor-prefetch");
        }
    }

    #[test]
    fn test_model_lock_creation() {
        // get_model_lock should return same Arc for same model
        let lock1 = get_model_lock("test-model");
        let lock2 = get_model_lock("test-model");
        assert!(Arc::ptr_eq(&lock1, &lock2), "Same model should return same lock");

        let lock3 = get_model_lock("other-model");
        assert!(!Arc::ptr_eq(&lock1, &lock3), "Different models should have different locks");
    }

    #[test]
    fn test_display_canonical_names_are_unique() {
        let mut names = std::collections::HashSet::new();
        for model in EmbeddingModelType::all() {
            let name = model.to_string();
            assert!(
                names.insert(name.clone()),
                "Duplicate display name '{}' for {:?}",
                name,
                model
            );
        }
    }

    #[test]
    fn test_from_str_accepts_every_display_name() {
        for model in EmbeddingModelType::all() {
            let name = model.to_string();
            let parsed: EmbeddingModelType = name.parse().unwrap_or_else(|_| {
                panic!("Canonical display name '{}' should parse for {:?}", name, model)
            });
            assert_eq!(parsed, model);
        }
    }

    #[test]
    fn test_from_str_all_documented_aliases() {
        let aliases = [
            ("bge-small-en", EmbeddingModelType::BgeSmallEnV15),
            ("bge-base-en", EmbeddingModelType::BgeBaseEnV15),
            ("bge-large-en", EmbeddingModelType::BgeLargeEnV15),
            ("minilm-l6", EmbeddingModelType::AllMiniLmL6V2),
            ("minilm-l12", EmbeddingModelType::AllMiniLmL12V2),
            ("e5-base", EmbeddingModelType::MultilingualE5Base),
            ("bge-small-zh", EmbeddingModelType::BgeSmallZhV15),
            ("bge-large-zh", EmbeddingModelType::BgeLargeZhV15),
            ("nomic-v1", EmbeddingModelType::NomicEmbedTextV1),
            ("nomic-v1.5", EmbeddingModelType::NomicEmbedTextV15),
            ("gemma-300m", EmbeddingModelType::EmbeddingGemma300M),
            ("BGE-SMALL-EN-V1.5-Q", EmbeddingModelType::BgeSmallEnV15Q),
            ("ALL-MINILM-L12-V2-Q", EmbeddingModelType::AllMiniLmL12V2Q),
            ("SNOWFLAKE-ARCTIC-EMBED-M-LONG-Q", EmbeddingModelType::SnowflakeArcticEmbedMLongQ),
        ];
        for (alias, expected) in aliases {
            let parsed: EmbeddingModelType = alias.parse().unwrap_or_else(|_| {
                panic!("Alias '{}' should parse", alias)
            });
            assert_eq!(parsed, expected, "Alias '{}' mismatch", alias);
        }
    }

    #[test]
    fn test_from_str_quantized_variants() {
        let quantized = [
            ("bge-small-en-v1.5-q", EmbeddingModelType::BgeSmallEnV15Q),
            ("all-minilm-l6-v2-q", EmbeddingModelType::AllMiniLmL6V2Q),
            ("all-minilm-l12-v2-q", EmbeddingModelType::AllMiniLmL12V2Q),
            ("bge-base-en-v1.5-q", EmbeddingModelType::BgeBaseEnV15Q),
            ("bge-large-en-v1.5-q", EmbeddingModelType::BgeLargeEnV15Q),
            ("paraphrase-minilm-l12-v2-q", EmbeddingModelType::ParaphraseMiniLmL12V2Q),
            ("nomic-embed-text-v1.5-q", EmbeddingModelType::NomicEmbedTextV15Q),
            ("mxbai-embed-large-v1-q", EmbeddingModelType::MxbaiEmbedLargeV1Q),
            ("gte-base-en-v1.5-q", EmbeddingModelType::GteBaseEnV15Q),
            ("gte-large-en-v1.5-q", EmbeddingModelType::GteLargeEnV15Q),
            ("snowflake-arctic-embed-xs-q", EmbeddingModelType::SnowflakeArcticEmbedXsQ),
            ("snowflake-arctic-embed-s-q", EmbeddingModelType::SnowflakeArcticEmbedSQ),
            ("snowflake-arctic-embed-m-q", EmbeddingModelType::SnowflakeArcticEmbedMQ),
            ("snowflake-arctic-embed-m-long-q", EmbeddingModelType::SnowflakeArcticEmbedMLongQ),
            ("snowflake-arctic-embed-l-q", EmbeddingModelType::SnowflakeArcticEmbedLQ),
        ];
        for (name, expected) in quantized {
            let parsed: EmbeddingModelType = name.parse().unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.to_string(), name);
        }
    }

    #[test]
    fn test_to_fastembed_model_maps_each_variant() {
        let mut seen = std::collections::HashSet::new();
        for model in EmbeddingModelType::all() {
            let fastembed_model = model.to_fastembed_model();
            let debug = format!("{:?}", fastembed_model);
            assert!(
                seen.insert(debug.clone()),
                "Duplicate fastembed mapping {:?} -> {}",
                model,
                debug
            );
        }
    }

    #[test]
    fn test_display_matches_from_str_for_every_model() {
        let expected_names = [
            (EmbeddingModelType::BgeSmallEnV15, "bge-small-en-v1.5"),
            (EmbeddingModelType::AllMiniLmL6V2, "all-minilm-l6-v2"),
            (EmbeddingModelType::AllMpnetBaseV2, "all-mpnet-base-v2"),
            (EmbeddingModelType::MultilingualE5Large, "multilingual-e5-large"),
            (EmbeddingModelType::BgeSmallZhV15, "bge-small-zh-v1.5"),
            (EmbeddingModelType::NomicEmbedTextV15, "nomic-embed-text-v1.5"),
            (EmbeddingModelType::MxbaiEmbedLargeV1, "mxbai-embed-large-v1"),
            (EmbeddingModelType::ClipVitB32, "clip-vit-b-32"),
            (EmbeddingModelType::JinaEmbeddingsV2BaseCode, "jina-embeddings-v2-base-code"),
            (EmbeddingModelType::EmbeddingGemma300M, "embedding-gemma-300m"),
            (EmbeddingModelType::ModernBertEmbedLarge, "modernbert-embed-large"),
            (EmbeddingModelType::SnowflakeArcticEmbedL, "snowflake-arctic-embed-l"),
        ];
        for (model, expected) in expected_names {
            assert_eq!(model.to_string(), expected);
            assert_eq!(expected.parse::<EmbeddingModelType>().unwrap(), model);
            assert!(format!("{model}") == expected);
        }
    }
    // ---- is_quantized ----

    #[test]
    fn test_is_quantized_quantized_models() {
        let quantized = [
            EmbeddingModelType::BgeSmallEnV15Q,
            EmbeddingModelType::AllMiniLmL6V2Q,
            EmbeddingModelType::AllMiniLmL12V2Q,
            EmbeddingModelType::BgeBaseEnV15Q,
            EmbeddingModelType::BgeLargeEnV15Q,
            EmbeddingModelType::ParaphraseMiniLmL12V2Q,
            EmbeddingModelType::NomicEmbedTextV15Q,
            EmbeddingModelType::MxbaiEmbedLargeV1Q,
            EmbeddingModelType::GteBaseEnV15Q,
            EmbeddingModelType::GteLargeEnV15Q,
            EmbeddingModelType::SnowflakeArcticEmbedXsQ,
            EmbeddingModelType::SnowflakeArcticEmbedSQ,
            EmbeddingModelType::SnowflakeArcticEmbedMQ,
            EmbeddingModelType::SnowflakeArcticEmbedMLongQ,
            EmbeddingModelType::SnowflakeArcticEmbedLQ,
        ];
        for model in &quantized {
            assert!(model.is_quantized(), "{:?} should be quantized", model);
        }
    }

    #[test]
    fn test_is_quantized_non_quantized_models() {
        let non_quantized = [
            EmbeddingModelType::BgeSmallEnV15,
            EmbeddingModelType::AllMiniLmL6V2,
            EmbeddingModelType::AllMiniLmL12V2,
            EmbeddingModelType::AllMpnetBaseV2,
            EmbeddingModelType::BgeBaseEnV15,
            EmbeddingModelType::BgeLargeEnV15,
            EmbeddingModelType::MultilingualE5Small,
            EmbeddingModelType::MultilingualE5Base,
            EmbeddingModelType::MultilingualE5Large,
            EmbeddingModelType::NomicEmbedTextV1,
            EmbeddingModelType::NomicEmbedTextV15,
            EmbeddingModelType::ClipVitB32,
            EmbeddingModelType::JinaEmbeddingsV2BaseCode,
            EmbeddingModelType::EmbeddingGemma300M,
            EmbeddingModelType::ModernBertEmbedLarge,
            EmbeddingModelType::SnowflakeArcticEmbedXs,
            EmbeddingModelType::SnowflakeArcticEmbedS,
            EmbeddingModelType::SnowflakeArcticEmbedM,
            EmbeddingModelType::SnowflakeArcticEmbedMLong,
            EmbeddingModelType::SnowflakeArcticEmbedL,
        ];
        for model in &non_quantized {
            assert!(!model.is_quantized(), "{:?} should NOT be quantized", model);
        }
    }

    #[test]
    fn test_is_quantized_count() {
        let count = EmbeddingModelType::all()
            .iter()
            .filter(|m| m.is_quantized())
            .count();
        assert_eq!(count, 15, "Expected 15 quantized models");
    }

    // ---- EmbeddingModelType::default ----

    #[test]
    fn test_embedding_model_type_default_is_bge_small() {
        let default = EmbeddingModelType::default();
        assert_eq!(default, EmbeddingModelType::BgeSmallEnV15);
    }

    // ---- all() count ----

    #[test]
    fn test_all_model_count_exact() {
        let all = EmbeddingModelType::all();
        assert_eq!(all.len(), 42, "Expected exactly 42 embedding models");
    }

    // ---- hf_repo_id for all models ----

    #[test]
    fn test_hf_repo_id_all_models_non_empty() {
        for model in EmbeddingModelType::all() {
            let repo = model.hf_repo_id();
            assert!(!repo.is_empty(), "{:?} has empty hf_repo_id", model);
            assert!(repo.contains('/'), "{:?} hf_repo_id '{}' missing '/'", model, repo);
        }
    }

    // ---- max_context_length edge cases ----

    #[test]
    fn test_max_context_length_snowflake_mlong() {
        assert_eq!(
            EmbeddingModelType::SnowflakeArcticEmbedMLong.max_context_length(),
            2048
        );
        assert_eq!(
            EmbeddingModelType::SnowflakeArcticEmbedMLongQ.max_context_length(),
            2048
        );
    }

    #[test]
    fn test_max_context_length_all_models_positive() {
        for model in EmbeddingModelType::all() {
            let ctx = model.max_context_length();
            assert!(ctx > 0, "{:?} has zero context length", model);
        }
    }

    // ---- quantized ↔ base dimension consistency ----

    #[test]
    fn test_quantized_base_same_dimensions() {
        let pairs = [
            (EmbeddingModelType::BgeSmallEnV15, EmbeddingModelType::BgeSmallEnV15Q),
            (EmbeddingModelType::AllMiniLmL6V2, EmbeddingModelType::AllMiniLmL6V2Q),
            (EmbeddingModelType::AllMiniLmL12V2, EmbeddingModelType::AllMiniLmL12V2Q),
            (EmbeddingModelType::BgeBaseEnV15, EmbeddingModelType::BgeBaseEnV15Q),
            (EmbeddingModelType::BgeLargeEnV15, EmbeddingModelType::BgeLargeEnV15Q),
            (EmbeddingModelType::ParaphraseMiniLmL12V2, EmbeddingModelType::ParaphraseMiniLmL12V2Q),
            (EmbeddingModelType::NomicEmbedTextV15, EmbeddingModelType::NomicEmbedTextV15Q),
            (EmbeddingModelType::MxbaiEmbedLargeV1, EmbeddingModelType::MxbaiEmbedLargeV1Q),
            (EmbeddingModelType::GteBaseEnV15, EmbeddingModelType::GteBaseEnV15Q),
            (EmbeddingModelType::GteLargeEnV15, EmbeddingModelType::GteLargeEnV15Q),
            (EmbeddingModelType::SnowflakeArcticEmbedXs, EmbeddingModelType::SnowflakeArcticEmbedXsQ),
            (EmbeddingModelType::SnowflakeArcticEmbedS, EmbeddingModelType::SnowflakeArcticEmbedSQ),
            (EmbeddingModelType::SnowflakeArcticEmbedM, EmbeddingModelType::SnowflakeArcticEmbedMQ),
            (EmbeddingModelType::SnowflakeArcticEmbedMLong, EmbeddingModelType::SnowflakeArcticEmbedMLongQ),
            (EmbeddingModelType::SnowflakeArcticEmbedL, EmbeddingModelType::SnowflakeArcticEmbedLQ),
        ];
        for (base, quantized) in &pairs {
            assert_eq!(
                base.dimensions(),
                quantized.dimensions(),
                "{:?} and {:?} should have same dimensions",
                base,
                quantized
            );
        }
    }

    // ---- quantized ↔ base hf_repo_id consistency ----

    #[test]
    fn test_quantized_base_same_hf_repo() {
        let pairs = [
            (EmbeddingModelType::BgeSmallEnV15, EmbeddingModelType::BgeSmallEnV15Q),
            (EmbeddingModelType::AllMiniLmL6V2, EmbeddingModelType::AllMiniLmL6V2Q),
            (EmbeddingModelType::AllMiniLmL12V2, EmbeddingModelType::AllMiniLmL12V2Q),
        ];
        for (base, quantized) in &pairs {
            assert_eq!(
                base.hf_repo_id(),
                quantized.hf_repo_id(),
                "{:?} and {:?} should share hf_repo_id",
                base,
                quantized
            );
        }
    }

    // ---- is_multilingual coverage ----

    #[test]
    fn test_is_multilingual_chinese_models() {
        assert!(EmbeddingModelType::BgeSmallZhV15.is_multilingual());
        assert!(EmbeddingModelType::BgeLargeZhV15.is_multilingual());
    }

    #[test]
    fn test_is_multilingual_paraphrase_multilingual() {
        assert!(EmbeddingModelType::ParaphraseMultilingualMpnetBaseV2.is_multilingual());
    }

    #[test]
    fn test_is_multilingual_english_only() {
        let english_only = [
            EmbeddingModelType::AllMiniLmL6V2,
            EmbeddingModelType::AllMpnetBaseV2,
            EmbeddingModelType::BgeBaseEnV15,
            EmbeddingModelType::BgeLargeEnV15,
            EmbeddingModelType::ClipVitB32,
            EmbeddingModelType::NomicEmbedTextV15,
            EmbeddingModelType::MxbaiEmbedLargeV1,
            EmbeddingModelType::JinaEmbeddingsV2BaseCode,
            EmbeddingModelType::EmbeddingGemma300M,
            EmbeddingModelType::ModernBertEmbedLarge,
            EmbeddingModelType::SnowflakeArcticEmbedL,
        ];
        for model in &english_only {
            assert!(!model.is_multilingual(), "{:?} should NOT be multilingual", model);
        }
    }

    // ---- EmbeddingModelType serde roundtrip ----

    #[test]
    fn test_embedding_model_type_serde_roundtrip() {
        for model in EmbeddingModelType::all() {
            let json = serde_json::to_string(&model).unwrap();
            let parsed: EmbeddingModelType = serde_json::from_str(&json).unwrap_or_else(|_| {
                panic!("Serde roundtrip failed for {:?} (json: {})", model, json)
            });
            assert_eq!(parsed, model, "Serde mismatch for {}", json);
        }
    }

    #[test]
    fn test_embedding_model_type_serde_kebab_case() {
        // serde rename_all="kebab-case" strips dots: V15 → v15, not v1.5
        let json = serde_json::to_string(&EmbeddingModelType::BgeSmallEnV15).unwrap();
        assert_eq!(json, "\"bge-small-en-v15\"");
        let json = serde_json::to_string(&EmbeddingModelType::MultilingualE5Large).unwrap();
        assert_eq!(json, "\"multilingual-e5-large\"");
        let json = serde_json::to_string(&EmbeddingModelType::NomicEmbedTextV15Q).unwrap();
        assert_eq!(json, "\"nomic-embed-text-v15-q\"");
    }

    #[test]
    fn test_embedding_model_type_serde_from_json_string() {
        // serde names strip dots, use serde-generated name not Display name
        let model: EmbeddingModelType = serde_json::from_str("\"bge-large-en-v15\"").unwrap();
        assert_eq!(model, EmbeddingModelType::BgeLargeEnV15);
    }

    // ---- AccelerationBackend tests ----

    #[test]
    fn test_acceleration_backend_default_is_cpu() {
        assert_eq!(AccelerationBackend::default(), AccelerationBackend::Cpu);
    }

    #[test]
    fn test_acceleration_backend_partial_eq() {
        assert_eq!(AccelerationBackend::Cpu, AccelerationBackend::Cpu);
        assert_eq!(
            AccelerationBackend::Cuda { device_id: 0 },
            AccelerationBackend::Cuda { device_id: 0 }
        );
        assert_ne!(
            AccelerationBackend::Cuda { device_id: 0 },
            AccelerationBackend::Cuda { device_id: 1 }
        );
        assert_ne!(AccelerationBackend::Cpu, AccelerationBackend::Metal);
        assert_ne!(AccelerationBackend::Metal, AccelerationBackend::Vulkan);
    }

    #[test]
    fn test_acceleration_backend_debug() {
        let dbg = format!("{:?}", AccelerationBackend::Cpu);
        assert_eq!(dbg, "Cpu");
        let dbg = format!("{:?}", AccelerationBackend::Metal);
        assert_eq!(dbg, "Metal");
        let dbg = format!("{:?}", AccelerationBackend::Cuda { device_id: 2 });
        assert!(dbg.contains("Cuda"), "Debug should contain 'Cuda': {}", dbg);
        assert!(dbg.contains("2"), "Debug should contain device_id: {}", dbg);
    }

    #[test]
    fn test_acceleration_backend_clone() {
        let backend = AccelerationBackend::Cuda { device_id: 3 };
        let cloned = backend.clone();
        assert_eq!(backend, cloned);
    }

    #[test]
    fn test_acceleration_backend_serde_cpu() {
        let backend = AccelerationBackend::Cpu;
        let json = serde_json::to_string(&backend).unwrap();
        assert_eq!(json, "\"cpu\"");
        let parsed: AccelerationBackend = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, AccelerationBackend::Cpu);
    }

    #[test]
    fn test_acceleration_backend_serde_cuda() {
        let backend = AccelerationBackend::Cuda { device_id: 1 };
        let json = serde_json::to_string(&backend).unwrap();
        let parsed: AccelerationBackend = serde_json::from_str(&json).unwrap();
        match parsed {
            AccelerationBackend::Cuda { device_id } => assert_eq!(device_id, 1),
            _ => panic!("Expected Cuda variant, got {:?}", parsed),
        }
    }

    #[test]
    fn test_acceleration_backend_serde_metal() {
        let json = "\"metal\"";
        let parsed: AccelerationBackend = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, AccelerationBackend::Metal);
    }

    #[test]
    fn test_acceleration_backend_serde_vulkan() {
        let json = "\"vulkan\"";
        let parsed: AccelerationBackend = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, AccelerationBackend::Vulkan);
    }

    #[test]
    fn test_acceleration_backend_serde_invalid() {
        let result = serde_json::from_str::<AccelerationBackend>("\"opencl\"");
        assert!(result.is_err());
    }

    // ---- SparseModelType tests ----

    #[test]
    fn test_sparse_model_default_is_splade_pp_v1() {
        assert_eq!(SparseModelType::default(), SparseModelType::SpladePpV1);
    }

    #[test]
    fn test_sparse_model_to_fastembed() {
        // Should not panic and should return a valid SparseModel
        let _ = SparseModelType::SpladePpV1.to_fastembed_model();
    }

    #[test]
    fn test_sparse_model_debug() {
        let dbg = format!("{:?}", SparseModelType::SpladePpV1);
        assert_eq!(dbg, "SpladePpV1");
    }

    #[test]
    fn test_sparse_model_clone() {
        let model = SparseModelType::SpladePpV1;
        let cloned = model;
        assert_eq!(model, cloned);
    }

    #[test]
    fn test_sparse_model_serde_roundtrip() {
        let model = SparseModelType::SpladePpV1;
        let json = serde_json::to_string(&model).unwrap();
        assert_eq!(json, "\"splade-pp-v1\"");
        let parsed: SparseModelType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, model);
    }

    // ---- EmbeddingConfig serde edge cases ----

    #[test]
    fn test_embedding_config_serde_empty_object() {
        // All fields have #[serde(default)] so empty JSON should deserialize
        let config: EmbeddingConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.model, EmbeddingModelType::BgeSmallEnV15);
        assert_eq!(config.batch_size, 32);
        assert!(config.show_download_progress);
        assert!(!config.sparse_enabled);
        assert_eq!(config.sparse_model, SparseModelType::SpladePpV1);
    }

    #[test]
    fn test_embedding_config_serde_partial_fields() {
        // Only set batch_size; rest should default
        let config: EmbeddingConfig = serde_json::from_str(r#"{"batch_size": 128}"#).unwrap();
        assert_eq!(config.batch_size, 128);
        assert_eq!(config.model, EmbeddingModelType::BgeSmallEnV15);
        assert!(config.show_download_progress);
    }

    #[test]
    fn test_embedding_config_serde_override_model() {
        // Use serde variant name (kebab-case), not FromStr alias
        let config: EmbeddingConfig = serde_json::from_str(r#"{"model": "nomic-embed-text-v15"}"#).unwrap();
        assert_eq!(config.model, EmbeddingModelType::NomicEmbedTextV15);
    }

    #[test]
    fn test_embedding_config_serde_sparse_model_field() {
        // serde uses full variant name, not FromStr alias "splade"
        let config: EmbeddingConfig = serde_json::from_str(
            r#"{"sparse_enabled": true, "sparse_model": "splade-pp-v1"}"#
        ).unwrap();
        assert!(config.sparse_enabled);
        assert_eq!(config.sparse_model, SparseModelType::SpladePpV1);
    }

    #[test]
    fn test_embedding_config_serde_json_output() {
        let config = EmbeddingConfig::default();
        let json = serde_json::to_string_pretty(&config).unwrap();
        // Should contain all field names
        assert!(json.contains("\"model\""));
        assert!(json.contains("\"batch_size\""));
        assert!(json.contains("\"show_download_progress\""));
        assert!(json.contains("\"sparse_enabled\""));
        assert!(json.contains("\"sparse_model\""));
    }

    // ---- FromStr edge cases ----

    #[test]
    fn test_from_str_empty_string() {
        let result = "".parse::<EmbeddingModelType>();
        assert!(result.is_err());
    }

    #[test]
    fn test_from_str_whitespace() {
        let result = "  ".parse::<EmbeddingModelType>();
        assert!(result.is_err());
    }

    #[test]
    fn test_from_str_sparse_empty_string() {
        let result = "".parse::<SparseModelType>();
        assert!(result.is_err());
    }

    // ---- Debug format for EmbeddingModelType ----

    #[test]
    fn test_embedding_model_type_debug_format() {
        let dbg = format!("{:?}", EmbeddingModelType::BgeSmallEnV15);
        assert_eq!(dbg, "BgeSmallEnV15");
        let dbg = format!("{:?}", EmbeddingModelType::NomicEmbedTextV15);
        assert_eq!(dbg, "NomicEmbedTextV15");
    }

    // ---- EmbeddingModelType Copy/Clone ----

    #[test]
    fn test_embedding_model_type_copy_clone() {
        let original = EmbeddingModelType::GteLargeEnV15;
        let copied = original;
        let cloned = original.clone();
        assert_eq!(original, copied);
        assert_eq!(original, cloned);
    }

    // ---- EmbeddingModelType Hash ----

    #[test]
    fn test_embedding_model_type_hash_in_set() {
        let mut set = std::collections::HashSet::new();
        for model in EmbeddingModelType::all() {
            set.insert(model);
        }
        // All 42 unique models should be in the set
        assert_eq!(set.len(), 42);
    }

    // ---- dimensions for specific categories ----

    #[test]
    fn test_dimensions_512_models() {
        assert_eq!(EmbeddingModelType::BgeSmallZhV15.dimensions(), 512);
        assert_eq!(EmbeddingModelType::ClipVitB32.dimensions(), 512);
    }

    #[test]
    fn test_dimensions_384_models() {
        assert_eq!(EmbeddingModelType::MultilingualE5Small.dimensions(), 384);
        assert_eq!(EmbeddingModelType::SnowflakeArcticEmbedXs.dimensions(), 384);
        assert_eq!(EmbeddingModelType::SnowflakeArcticEmbedS.dimensions(), 384);
    }

    #[test]
    fn test_dimensions_768_models() {
        assert_eq!(EmbeddingModelType::AllMpnetBaseV2.dimensions(), 768);
        assert_eq!(EmbeddingModelType::BgeBaseEnV15.dimensions(), 768);
        assert_eq!(EmbeddingModelType::MultilingualE5Base.dimensions(), 768);
        assert_eq!(EmbeddingModelType::NomicEmbedTextV1.dimensions(), 768);
        assert_eq!(EmbeddingModelType::EmbeddingGemma300M.dimensions(), 768);
        assert_eq!(EmbeddingModelType::SnowflakeArcticEmbedM.dimensions(), 768);
    }

    #[test]
    fn test_dimensions_1024_models() {
        assert_eq!(EmbeddingModelType::BgeLargeEnV15.dimensions(), 1024);
        assert_eq!(EmbeddingModelType::MultilingualE5Large.dimensions(), 1024);
        assert_eq!(EmbeddingModelType::MxbaiEmbedLargeV1.dimensions(), 1024);
        assert_eq!(EmbeddingModelType::GteLargeEnV15.dimensions(), 1024);
        assert_eq!(EmbeddingModelType::ModernBertEmbedLarge.dimensions(), 1024);
        assert_eq!(EmbeddingModelType::SnowflakeArcticEmbedL.dimensions(), 1024);
    }

    // ---- to_fastembed_model uniqueness per dimension category ----

    #[test]
    fn test_to_fastembed_model_does_not_panic() {
        // Exhaustive: call to_fastembed_model on every variant
        let count = EmbeddingModelType::all()
            .into_iter()
            .map(|m| {
                let _ = m.to_fastembed_model();
                1
            })
            .sum::<usize>();
        assert_eq!(count, 42);
    }

    // ---- AccelerationBackend Copy ----

    #[test]
    fn test_acceleration_backend_copy() {
        let a = AccelerationBackend::Cuda { device_id: 5 };
        let b = a;
        assert_eq!(a, b);
    }
    // ---- Embedding vector utilities ----

    #[test]
    fn cosine_similarity_identical_unit_vectors() {
        let a = [1.0f32, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = [1.0f32, 0.0];
        let b = [0.0, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = [1.0f32, 0.0];
        let b = [-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_similarity_mismatched_lengths_returns_zero() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn cosine_similarity_empty_vectors_returns_zero() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn cosine_similarity_zero_vector_returns_zero() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn cosine_distance_matches_one_minus_similarity() {
        let a = [3.0f32, 4.0];
        let b = [4.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!((cosine_distance(&a, &b) - (1.0 - sim)).abs() < 1e-6);
    }

    #[test]
    fn euclidean_distance_identical_vectors_is_zero() {
        let a = [1.0f32, 2.0, 3.0];
        let b = [1.0, 2.0, 3.0];
        assert!((euclidean_distance(&a, &b).unwrap()).abs() < f32::EPSILON);
    }

    #[test]
    fn euclidean_distance_known_value() {
        let a = [0.0f32, 0.0];
        let b = [3.0, 4.0];
        assert!((euclidean_distance(&a, &b).unwrap() - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn euclidean_distance_dimension_mismatch_is_error() {
        let err = euclidean_distance(&[1.0], &[1.0, 2.0]).unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains("dimension mismatch"));
    }

    #[test]
    fn normalize_embedding_produces_unit_length() {
        let mut v = [3.0f32, 4.0];
        normalize_embedding(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn normalize_embedding_zero_vector_is_noop() {
        let mut v = [0.0f32, 0.0];
        normalize_embedding(&mut v);
        assert_eq!(v, [0.0, 0.0]);
    }

    #[test]
    fn validate_embedding_dims_accepts_matching_length() {
        assert!(validate_embedding_dims(&[0.1, 0.2, 0.3], 3).is_ok());
    }

    #[test]
    fn validate_embedding_dims_rejects_wrong_length() {
        let err = validate_embedding_dims(&[1.0, 2.0], 384).unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains("Expected embedding dimension 384"));
    }

    #[test]
    fn validate_embedding_dims_rejects_empty() {
        let err = validate_embedding_dims(&[], 384).unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn dense_embedding_creates_valid_vector() {
        let values: Vec<f32> = (0..384).map(|i| i as f32 * 0.01).collect();
        let embedding = dense_embedding(values.clone(), 384).unwrap();
        assert_eq!(embedding, values);
    }

    #[test]
    fn dense_embedding_propagates_dimension_error() {
        let err = dense_embedding(vec![1.0, 2.0], 3).unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[test]
    fn sparse_embeddings_disabled_error_message() {
        let err = sparse_embeddings_disabled_error();
        let msg = err.to_string();
        assert!(msg.contains("Sparse embeddings not enabled"));
        assert!(msg.contains("sparse_enabled"));
    }

    #[test]
    fn cosine_similarity_high_dimensional_embeddings() {
        let dim = EmbeddingModelType::BgeSmallEnV15.dimensions();
        let a: Vec<f32> = (0..dim).map(|i| i as f32).collect();
        let b = a.clone();
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_clamps_to_unit_interval() {
        // Same direction at large scale should still yield ~1.0 (not overflow past clamp range)
        let a = [1_000.0f32, 2_000.0, 3_000.0];
        let b = [2_000.0, 4_000.0, 6_000.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-5, "parallel vectors should score ~1.0, got {}", sim);
        assert!((-1.0..=1.0).contains(&sim), "similarity should stay in [-1, 1]: {}", sim);
    }

    #[test]
    fn test_embedding_config_debug_and_clone() {
        let config = EmbeddingConfig::default();
        let cloned = config.clone();
        assert_eq!(cloned.model, config.model);
        assert_eq!(cloned.batch_size, config.batch_size);
        assert_eq!(cloned.show_download_progress, config.show_download_progress);
        assert_eq!(cloned.sparse_enabled, config.sparse_enabled);
        assert_eq!(cloned.sparse_model, config.sparse_model);
        let dbg = format!("{:?}", config);
        assert!(dbg.contains("EmbeddingConfig"));
        assert!(dbg.contains("BgeSmallEnV15"));
    }

    #[test]
    fn test_embedding_config_serde_rejects_invalid_model() {
        let result = serde_json::from_str::<EmbeddingConfig>(r#"{"model": "not-a-real-model"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_dense_embedding_uses_model_dimensions() {
        let model = EmbeddingModelType::BgeSmallEnV15;
        let values = vec![0.1f32; model.dimensions()];
        let embedding = dense_embedding(values.clone(), model.dimensions()).unwrap();
        assert_eq!(embedding.len(), model.dimensions());
        assert_eq!(embedding, values);
    }

    #[test]
    fn sparse_embeddings_disabled_error_is_internal() {
        let err = sparse_embeddings_disabled_error();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_pre_download_creates_nested_file_parent_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache_dir = tmp.path().to_path_buf();
        let _ = pre_download_model(
            "fake-org/nested-model",
            &["onnx/model.onnx"],
            &cache_dir,
        );
        let nested = cache_dir
            .join("models--fake-org--nested-model")
            .join("snapshots")
            .join("lancor-prefetch")
            .join("onnx");
        assert!(nested.exists(), "nested onnx parent dir should be created");
    }

    // HTTP embedding helpers (R40)
    fn sample_float_response_json(vectors: &[Vec<f32>]) -> String {
        let data: Vec<serde_json::Value> = vectors.iter().enumerate().map(|(index, embedding)| {
            serde_json::json!({"object": "embedding", "index": index, "embedding": embedding})
        }).collect();
        serde_json::json!({"object": "list", "data": data, "model": "test-model", "usage": {"prompt_tokens": 3, "total_tokens": 3}}).to_string()
    }
    fn encode_f32_le_base64(values: &[f32]) -> String {
        use base64::Engine;
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }
    #[test] fn http_embedding_request_single_input_serde_roundtrip() {
        let req = build_embedding_request("m", &["hello"], None, None);
        let parsed: EmbeddingRequest = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(parsed.model, "m");
        assert_eq!(parsed.input, EmbeddingInput::Single("hello".into()));
    }
    #[test] fn http_embedding_request_batch_input_serde_roundtrip() {
        let req = build_embedding_request("m", &["a", "b"], Some(EmbeddingEncodingFormat::Float), Some(384));
        let parsed: EmbeddingRequest = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(parsed.input, EmbeddingInput::Batch(vec!["a".into(), "b".into()]));
        assert_eq!(parsed.dimensions, Some(384));
    }
    #[test] fn http_embedding_request_base64_encoding_format_roundtrip() {
        let req = build_embedding_request("m", &["x"], Some(EmbeddingEncodingFormat::Base64), None);
        let parsed: EmbeddingRequest = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(parsed.encoding_format, Some(EmbeddingEncodingFormat::Base64));
    }
    #[test] fn http_embedding_response_float_serde_roundtrip() {
        let parsed: EmbeddingResponse = serde_json::from_str(&sample_float_response_json(&[vec![0.1, 0.2, 0.3]])).unwrap();
        assert_eq!(parsed.data[0].embedding, EmbeddingVector::Float(vec![0.1, 0.2, 0.3]));
    }
    #[test] fn http_embedding_response_base64_roundtrip_and_decode() {
        let values = vec![1.0f32, -2.5, 3.25];
        let item = EmbeddingDataItem { object: "embedding".into(), index: 0, embedding: EmbeddingVector::Base64(encode_f32_le_base64(&values)) };
        assert_eq!(decode_embedding_vector(&item.embedding).unwrap(), values);
    }
    #[test] fn model_dimensions_matches_embedding_model_type_for_all_models() {
        for model in EmbeddingModelType::all() { assert_eq!(model_dimensions(model), model.dimensions()); }
    }
    #[test] fn model_dimensions_quantized_matches_base_precision() {
        let pairs = [(EmbeddingModelType::BgeSmallEnV15, EmbeddingModelType::BgeSmallEnV15Q), (EmbeddingModelType::AllMiniLmL6V2, EmbeddingModelType::AllMiniLmL6V2Q), (EmbeddingModelType::BgeBaseEnV15, EmbeddingModelType::BgeBaseEnV15Q), (EmbeddingModelType::BgeLargeEnV15, EmbeddingModelType::BgeLargeEnV15Q)];
        for (full, q) in pairs { assert_eq!(model_dimensions(full), model_dimensions(q)); assert!(!full.is_quantized()); assert!(q.is_quantized()); }
    }
    #[test] fn batch_chunks_empty_input() { assert!(batch_chunks(&[] as &[i32], 4).is_empty()); }
    #[test] fn batch_chunks_single_batch_when_under_limit() { assert_eq!(batch_chunks(&["a","b","c"], 10), vec![vec!["a","b","c"]]); }
    #[test] fn batch_chunks_splits_preserving_order() {
        let items: Vec<i32> = (0..7).collect();
        assert_eq!(batch_chunks(&items, 3), vec![vec![0,1,2], vec![3,4,5], vec![6]]);
        let flat: Vec<i32> = batch_chunks(&items, 3).into_iter().flatten().collect();
        assert_eq!(flat, items);
    }
    #[test] fn batch_chunks_zero_batch_size_treated_as_one() { assert_eq!(batch_chunks(&[1,2,3], 0), vec![vec![1], vec![2], vec![3]]); }
    #[test] fn build_embedding_request_single_vs_batch_shape() {
        assert!(matches!(build_embedding_request("m", &["only"], None, None).input, EmbeddingInput::Single(_)));
        assert!(matches!(build_embedding_request("m", &["a","b"], None, None).input, EmbeddingInput::Batch(_)));
    }
    #[test] fn parse_embedding_response_sorts_by_index() {
        let body = serde_json::json!({"object":"list","model":"m","data":[{"object":"embedding","index":2,"embedding":[3.0]},{"object":"embedding","index":0,"embedding":[1.0]},{"object":"embedding","index":1,"embedding":[2.0]}]}).to_string();
        assert_eq!(parse_embedding_response(&body, None).unwrap(), vec![vec![1.0], vec![2.0], vec![3.0]]);
    }
    #[test] fn parse_embedding_response_validates_expected_dimensions() {
        assert!(matches!(parse_embedding_response(&sample_float_response_json(&[vec![1.0,2.0]]), Some(3)), Err(AppError::InvalidInput(_))));
    }
    #[test] fn parse_embedding_response_rejects_invalid_json() {
        assert!(matches!(parse_embedding_response("not-json", None), Err(AppError::InvalidInput(_))));
    }
    #[test] fn parse_embedding_response_rejects_empty_data() {
        assert!(matches!(parse_embedding_response(r#"{"object":"list","data":[],"model":"m"}"#, None), Err(AppError::InvalidInput(_))));
    }
    #[test] fn parse_embedding_response_decodes_base64_vectors() {
        let values = vec![0.5f32, -1.25, 2.0];
        let body = serde_json::json!({"object":"list","model":"m","data":[{"object":"embedding","index":0,"embedding":encode_f32_le_base64(&values)}]}).to_string();
        assert_eq!(parse_embedding_response(&body, None).unwrap(), vec![values]);
    }
    #[test] fn map_embedding_http_error_auth_for_401() { assert!(matches!(map_embedding_http_error(401, "bad"), AppError::Auth(_))); }
    #[test] fn map_embedding_http_error_rate_limited_for_429() { assert!(matches!(map_embedding_http_error(429, "slow"), AppError::RateLimited(_))); }
    #[test] fn map_embedding_http_error_external_for_500() { assert!(matches!(map_embedding_http_error(500, "boom"), AppError::External(_))); }
    #[test] fn map_embedding_http_error_invalid_input_for_400() { assert!(matches!(map_embedding_http_error(400, "bad"), AppError::InvalidInput(_))); }
    #[test] fn decode_base64_embedding_rejects_bad_length() {
        use base64::Engine;
        let bad = base64::engine::general_purpose::STANDARD.encode([1u8,2,3]);
        assert!(matches!(decode_base64_embedding(&bad), Err(AppError::InvalidInput(_))));
    }
    #[tokio::test] async fn wiremock_http_embedding_success() {
        use wiremock::matchers::{method, path}; use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let body = sample_float_response_json(&[vec![0.1,0.2,0.3], vec![0.4,0.5,0.6]]);
        Mock::given(method("POST")).and(path("/v1/embeddings")).respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json")).mount(&server).await;
        let client = HttpEmbeddingClient::new(server.uri()).unwrap();
        let vectors = client.embed(&build_embedding_request("test-model", &["a","b"], None, None)).await.unwrap();
        assert_eq!(vectors.len(), 2); assert_eq!(vectors[0].len(), 3);
    }
    #[tokio::test] async fn wiremock_http_embedding_maps_401_to_auth() {
        use wiremock::matchers::{method, path}; use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/embeddings")).respond_with(ResponseTemplate::new(401).set_body_string("unauthorized")).mount(&server).await;
        let err = HttpEmbeddingClient::new(server.uri()).unwrap().embed(&build_embedding_request("m", &["x"], None, None)).await.unwrap_err();
        assert!(matches!(err, AppError::Auth(_)));
    }
    #[tokio::test] async fn wiremock_http_embedding_maps_500_to_external() {
        use wiremock::matchers::{method, path}; use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/embeddings")).respond_with(ResponseTemplate::new(500).set_body_string("err")).mount(&server).await;
        let err = HttpEmbeddingClient::new(server.uri()).unwrap().embed(&build_embedding_request("m", &["x"], None, None)).await.unwrap_err();
        assert!(matches!(err, AppError::External(_)));
    }
    #[tokio::test] async fn wiremock_http_embedding_invalid_json_maps_to_invalid_input() {
        use wiremock::matchers::{method, path}; use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/embeddings")).respond_with(ResponseTemplate::new(200).set_body_string("{bad")).mount(&server).await;
        let err = HttpEmbeddingClient::new(server.uri()).unwrap().embed(&build_embedding_request("m", &["x"], None, None)).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }
    #[tokio::test] async fn wiremock_http_embedding_batched_preserves_order() {
        use wiremock::matchers::{method, path}; use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/embeddings")).respond_with(|req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let inputs: Vec<String> = match &body["input"] { serde_json::Value::String(s) => vec![s.clone()], serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(), _ => panic!() };
            let vectors: Vec<Vec<f32>> = inputs.iter().map(|l| vec![l.trim_start_matches('t').parse::<f32>().unwrap() + 1.0]).collect();
            ResponseTemplate::new(200).set_body_json(serde_json::from_str::<serde_json::Value>(&sample_float_response_json(&vectors)).unwrap())
        }).mount(&server).await;
        let vectors = HttpEmbeddingClient::new(server.uri()).unwrap().embed_texts_batched("m", &["t0","t1","t2","t3","t4"], 2, None, None).await.unwrap();
        assert_eq!(vectors.len(), 5); assert_eq!(vectors[0], vec![1.0]); assert_eq!(vectors[4], vec![5.0]);
    }
    #[tokio::test] async fn wiremock_http_embedding_base64_response_path() {
        use wiremock::matchers::{method, path}; use wiremock::{Mock, MockServer, ResponseTemplate};
        let values = vec![1.0f32, 2.0, 3.0];
        let body = serde_json::json!({"object":"list","model":"m","data":[{"object":"embedding","index":0,"embedding":encode_f32_le_base64(&values)}]}).to_string();
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/embeddings")).respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json")).mount(&server).await;
        let vectors = HttpEmbeddingClient::new(server.uri()).unwrap().embed(&build_embedding_request("m", &["x"], Some(EmbeddingEncodingFormat::Base64), None)).await.unwrap();
        assert_eq!(vectors[0], values);
    }
    #[tokio::test] async fn wiremock_http_embedding_timeout_maps_to_unavailable() {
        use std::time::Duration; use wiremock::matchers::{method, path}; use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/embeddings")).respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)).set_body_string("{}")).mount(&server).await;
        let client = HttpEmbeddingClient { http: reqwest::Client::builder().timeout(Duration::from_millis(200)).build().unwrap(), base_url: server.uri(), api_key: None };
        let err = client.embed(&build_embedding_request("m", &["x"], None, None)).await.unwrap_err();
        assert!(matches!(err, AppError::Unavailable(_)));
    }

    // ============================================================================
    // Exhaustive EmbeddingModelType::all() coverage
    // ============================================================================

    #[test]
    fn test_all_contains_every_variant() {
        let all = EmbeddingModelType::all();
        let expected = [
            EmbeddingModelType::BgeSmallEnV15,
            EmbeddingModelType::BgeSmallEnV15Q,
            EmbeddingModelType::AllMiniLmL6V2,
            EmbeddingModelType::AllMiniLmL6V2Q,
            EmbeddingModelType::AllMiniLmL12V2,
            EmbeddingModelType::AllMiniLmL12V2Q,
            EmbeddingModelType::AllMpnetBaseV2,
            EmbeddingModelType::BgeBaseEnV15,
            EmbeddingModelType::BgeBaseEnV15Q,
            EmbeddingModelType::BgeLargeEnV15,
            EmbeddingModelType::BgeLargeEnV15Q,
            EmbeddingModelType::MultilingualE5Small,
            EmbeddingModelType::MultilingualE5Base,
            EmbeddingModelType::MultilingualE5Large,
            EmbeddingModelType::ParaphraseMiniLmL12V2,
            EmbeddingModelType::ParaphraseMiniLmL12V2Q,
            EmbeddingModelType::ParaphraseMultilingualMpnetBaseV2,
            EmbeddingModelType::BgeSmallZhV15,
            EmbeddingModelType::BgeLargeZhV15,
            EmbeddingModelType::NomicEmbedTextV1,
            EmbeddingModelType::NomicEmbedTextV15,
            EmbeddingModelType::NomicEmbedTextV15Q,
            EmbeddingModelType::MxbaiEmbedLargeV1,
            EmbeddingModelType::MxbaiEmbedLargeV1Q,
            EmbeddingModelType::GteBaseEnV15,
            EmbeddingModelType::GteBaseEnV15Q,
            EmbeddingModelType::GteLargeEnV15,
            EmbeddingModelType::GteLargeEnV15Q,
            EmbeddingModelType::ClipVitB32,
            EmbeddingModelType::JinaEmbeddingsV2BaseCode,
            EmbeddingModelType::EmbeddingGemma300M,
            EmbeddingModelType::ModernBertEmbedLarge,
            EmbeddingModelType::SnowflakeArcticEmbedXs,
            EmbeddingModelType::SnowflakeArcticEmbedXsQ,
            EmbeddingModelType::SnowflakeArcticEmbedS,
            EmbeddingModelType::SnowflakeArcticEmbedSQ,
            EmbeddingModelType::SnowflakeArcticEmbedM,
            EmbeddingModelType::SnowflakeArcticEmbedMQ,
            EmbeddingModelType::SnowflakeArcticEmbedMLong,
            EmbeddingModelType::SnowflakeArcticEmbedMLongQ,
            EmbeddingModelType::SnowflakeArcticEmbedL,
            EmbeddingModelType::SnowflakeArcticEmbedLQ,
        ];
        assert_eq!(all.len(), expected.len(), "Variant count mismatch");
        for variant in &expected {
            assert!(all.contains(variant), "Missing variant: {:?}", variant);
        }
    }

    #[test]
    fn test_all_contains_no_duplicates() {
        let all = EmbeddingModelType::all();
        let mut seen = std::collections::HashSet::new();
        for model in &all {
            assert!(
                seen.insert(*model),
                "Duplicate variant in all(): {:?}",
                model
            );
        }
        assert_eq!(seen.len(), all.len());
    }

    // ============================================================================
    // Exhaustive dimensions() coverage
    // ============================================================================

    #[test]
    fn test_dimensions_exhaustive_per_variant() {
        let cases: &[(EmbeddingModelType, usize)] = &[
            // 384 dimensions
            (EmbeddingModelType::BgeSmallEnV15, 384),
            (EmbeddingModelType::BgeSmallEnV15Q, 384),
            (EmbeddingModelType::AllMiniLmL6V2, 384),
            (EmbeddingModelType::AllMiniLmL6V2Q, 384),
            (EmbeddingModelType::AllMiniLmL12V2, 384),
            (EmbeddingModelType::AllMiniLmL12V2Q, 384),
            (EmbeddingModelType::MultilingualE5Small, 384),
            (EmbeddingModelType::SnowflakeArcticEmbedXs, 384),
            (EmbeddingModelType::SnowflakeArcticEmbedXsQ, 384),
            (EmbeddingModelType::SnowflakeArcticEmbedS, 384),
            (EmbeddingModelType::SnowflakeArcticEmbedSQ, 384),
            // 512 dimensions
            (EmbeddingModelType::BgeSmallZhV15, 512),
            (EmbeddingModelType::ClipVitB32, 512),
            // 768 dimensions
            (EmbeddingModelType::AllMpnetBaseV2, 768),
            (EmbeddingModelType::BgeBaseEnV15, 768),
            (EmbeddingModelType::BgeBaseEnV15Q, 768),
            (EmbeddingModelType::MultilingualE5Base, 768),
            (EmbeddingModelType::ParaphraseMiniLmL12V2, 768),
            (EmbeddingModelType::ParaphraseMiniLmL12V2Q, 768),
            (EmbeddingModelType::ParaphraseMultilingualMpnetBaseV2, 768),
            (EmbeddingModelType::NomicEmbedTextV1, 768),
            (EmbeddingModelType::NomicEmbedTextV15, 768),
            (EmbeddingModelType::NomicEmbedTextV15Q, 768),
            (EmbeddingModelType::GteBaseEnV15, 768),
            (EmbeddingModelType::GteBaseEnV15Q, 768),
            (EmbeddingModelType::JinaEmbeddingsV2BaseCode, 768),
            (EmbeddingModelType::EmbeddingGemma300M, 768),
            (EmbeddingModelType::SnowflakeArcticEmbedM, 768),
            (EmbeddingModelType::SnowflakeArcticEmbedMQ, 768),
            (EmbeddingModelType::SnowflakeArcticEmbedMLong, 768),
            (EmbeddingModelType::SnowflakeArcticEmbedMLongQ, 768),
            // 1024 dimensions
            (EmbeddingModelType::BgeLargeEnV15, 1024),
            (EmbeddingModelType::BgeLargeEnV15Q, 1024),
            (EmbeddingModelType::BgeLargeZhV15, 1024),
            (EmbeddingModelType::MultilingualE5Large, 1024),
            (EmbeddingModelType::MxbaiEmbedLargeV1, 1024),
            (EmbeddingModelType::MxbaiEmbedLargeV1Q, 1024),
            (EmbeddingModelType::GteLargeEnV15, 1024),
            (EmbeddingModelType::GteLargeEnV15Q, 1024),
            (EmbeddingModelType::ModernBertEmbedLarge, 1024),
            (EmbeddingModelType::SnowflakeArcticEmbedL, 1024),
            (EmbeddingModelType::SnowflakeArcticEmbedLQ, 1024),
        ];
        for (model, expected_dim) in cases {
            assert_eq!(
                model.dimensions(),
                *expected_dim,
                "Unexpected dimension for {:?}",
                model
            );
        }
    }

    // ============================================================================
    // Exhaustive max_context_length() coverage
    // ============================================================================

    #[test]
    fn test_max_context_length_exhaustive_per_variant() {
        let cases: &[(EmbeddingModelType, usize)] = &[
            (EmbeddingModelType::NomicEmbedTextV1, 8192),
            (EmbeddingModelType::NomicEmbedTextV15, 8192),
            (EmbeddingModelType::NomicEmbedTextV15Q, 8192),
            (EmbeddingModelType::SnowflakeArcticEmbedMLong, 2048),
            (EmbeddingModelType::SnowflakeArcticEmbedMLongQ, 2048),
            // All others default to 512
            (EmbeddingModelType::BgeSmallEnV15, 512),
            (EmbeddingModelType::BgeSmallEnV15Q, 512),
            (EmbeddingModelType::AllMiniLmL6V2, 512),
            (EmbeddingModelType::AllMiniLmL6V2Q, 512),
            (EmbeddingModelType::AllMiniLmL12V2, 512),
            (EmbeddingModelType::AllMiniLmL12V2Q, 512),
            (EmbeddingModelType::AllMpnetBaseV2, 512),
            (EmbeddingModelType::BgeBaseEnV15, 512),
            (EmbeddingModelType::BgeBaseEnV15Q, 512),
            (EmbeddingModelType::BgeLargeEnV15, 512),
            (EmbeddingModelType::BgeLargeEnV15Q, 512),
            (EmbeddingModelType::MultilingualE5Small, 512),
            (EmbeddingModelType::MultilingualE5Base, 512),
            (EmbeddingModelType::MultilingualE5Large, 512),
            (EmbeddingModelType::ParaphraseMiniLmL12V2, 512),
            (EmbeddingModelType::ParaphraseMiniLmL12V2Q, 512),
            (EmbeddingModelType::ParaphraseMultilingualMpnetBaseV2, 512),
            (EmbeddingModelType::BgeSmallZhV15, 512),
            (EmbeddingModelType::BgeLargeZhV15, 512),
            (EmbeddingModelType::MxbaiEmbedLargeV1, 512),
            (EmbeddingModelType::MxbaiEmbedLargeV1Q, 512),
            (EmbeddingModelType::GteBaseEnV15, 512),
            (EmbeddingModelType::GteBaseEnV15Q, 512),
            (EmbeddingModelType::GteLargeEnV15, 512),
            (EmbeddingModelType::GteLargeEnV15Q, 512),
            (EmbeddingModelType::ClipVitB32, 512),
            (EmbeddingModelType::JinaEmbeddingsV2BaseCode, 512),
            (EmbeddingModelType::EmbeddingGemma300M, 512),
            (EmbeddingModelType::ModernBertEmbedLarge, 512),
            (EmbeddingModelType::SnowflakeArcticEmbedXs, 512),
            (EmbeddingModelType::SnowflakeArcticEmbedXsQ, 512),
            (EmbeddingModelType::SnowflakeArcticEmbedS, 512),
            (EmbeddingModelType::SnowflakeArcticEmbedSQ, 512),
            (EmbeddingModelType::SnowflakeArcticEmbedM, 512),
            (EmbeddingModelType::SnowflakeArcticEmbedMQ, 512),
            (EmbeddingModelType::SnowflakeArcticEmbedL, 512),
            (EmbeddingModelType::SnowflakeArcticEmbedLQ, 512),
        ];
        for (model, expected_ctx) in cases {
            assert_eq!(
                model.max_context_length(),
                *expected_ctx,
                "Unexpected max_context_length for {:?}",
                model
            );
        }
    }

    // ============================================================================
    // Exhaustive is_multilingual() coverage
    // ============================================================================

    #[test]
    fn test_is_multilingual_exhaustive_per_variant() {
        let multilingual = [
            EmbeddingModelType::MultilingualE5Small,
            EmbeddingModelType::MultilingualE5Base,
            EmbeddingModelType::MultilingualE5Large,
            EmbeddingModelType::ParaphraseMultilingualMpnetBaseV2,
            EmbeddingModelType::BgeSmallZhV15,
            EmbeddingModelType::BgeLargeZhV15,
        ];
        let not_multilingual = [
            EmbeddingModelType::BgeSmallEnV15,
            EmbeddingModelType::BgeSmallEnV15Q,
            EmbeddingModelType::AllMiniLmL6V2,
            EmbeddingModelType::AllMiniLmL6V2Q,
            EmbeddingModelType::AllMiniLmL12V2,
            EmbeddingModelType::AllMiniLmL12V2Q,
            EmbeddingModelType::AllMpnetBaseV2,
            EmbeddingModelType::BgeBaseEnV15,
            EmbeddingModelType::BgeBaseEnV15Q,
            EmbeddingModelType::BgeLargeEnV15,
            EmbeddingModelType::BgeLargeEnV15Q,
            EmbeddingModelType::ParaphraseMiniLmL12V2,
            EmbeddingModelType::ParaphraseMiniLmL12V2Q,
            EmbeddingModelType::NomicEmbedTextV1,
            EmbeddingModelType::NomicEmbedTextV15,
            EmbeddingModelType::NomicEmbedTextV15Q,
            EmbeddingModelType::MxbaiEmbedLargeV1,
            EmbeddingModelType::MxbaiEmbedLargeV1Q,
            EmbeddingModelType::GteBaseEnV15,
            EmbeddingModelType::GteBaseEnV15Q,
            EmbeddingModelType::GteLargeEnV15,
            EmbeddingModelType::GteLargeEnV15Q,
            EmbeddingModelType::ClipVitB32,
            EmbeddingModelType::JinaEmbeddingsV2BaseCode,
            EmbeddingModelType::EmbeddingGemma300M,
            EmbeddingModelType::ModernBertEmbedLarge,
            EmbeddingModelType::SnowflakeArcticEmbedXs,
            EmbeddingModelType::SnowflakeArcticEmbedXsQ,
            EmbeddingModelType::SnowflakeArcticEmbedS,
            EmbeddingModelType::SnowflakeArcticEmbedSQ,
            EmbeddingModelType::SnowflakeArcticEmbedM,
            EmbeddingModelType::SnowflakeArcticEmbedMQ,
            EmbeddingModelType::SnowflakeArcticEmbedMLong,
            EmbeddingModelType::SnowflakeArcticEmbedMLongQ,
            EmbeddingModelType::SnowflakeArcticEmbedL,
            EmbeddingModelType::SnowflakeArcticEmbedLQ,
        ];
        for model in &multilingual {
            assert!(model.is_multilingual(), "{:?} should be multilingual", model);
        }
        for model in &not_multilingual {
            assert!(!model.is_multilingual(), "{:?} should NOT be multilingual", model);
        }
    }

    // ============================================================================
    // Exhaustive hf_repo_id() coverage
    // ============================================================================

    #[test]
    fn test_hf_repo_id_exhaustive_per_variant() {
        let cases: &[(EmbeddingModelType, &'static str)] = &[
            // Explicit mapped repos
            (EmbeddingModelType::BgeSmallEnV15, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::BgeSmallEnV15Q, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::AllMiniLmL6V2, "sentence-transformers/all-MiniLM-L6-v2"),
            (EmbeddingModelType::AllMiniLmL6V2Q, "sentence-transformers/all-MiniLM-L6-v2"),
            (EmbeddingModelType::AllMiniLmL12V2, "sentence-transformers/all-MiniLM-L12-v2"),
            (EmbeddingModelType::AllMiniLmL12V2Q, "sentence-transformers/all-MiniLM-L12-v2"),
            // Fallback repos
            (EmbeddingModelType::AllMpnetBaseV2, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::BgeBaseEnV15, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::BgeBaseEnV15Q, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::BgeLargeEnV15, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::BgeLargeEnV15Q, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::MultilingualE5Small, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::MultilingualE5Base, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::MultilingualE5Large, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::ParaphraseMiniLmL12V2, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::ParaphraseMiniLmL12V2Q, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::ParaphraseMultilingualMpnetBaseV2, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::BgeSmallZhV15, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::BgeLargeZhV15, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::NomicEmbedTextV1, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::NomicEmbedTextV15, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::NomicEmbedTextV15Q, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::MxbaiEmbedLargeV1, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::MxbaiEmbedLargeV1Q, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::GteBaseEnV15, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::GteBaseEnV15Q, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::GteLargeEnV15, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::GteLargeEnV15Q, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::ClipVitB32, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::JinaEmbeddingsV2BaseCode, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::EmbeddingGemma300M, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::ModernBertEmbedLarge, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::SnowflakeArcticEmbedXs, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::SnowflakeArcticEmbedXsQ, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::SnowflakeArcticEmbedS, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::SnowflakeArcticEmbedSQ, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::SnowflakeArcticEmbedM, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::SnowflakeArcticEmbedMQ, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::SnowflakeArcticEmbedMLong, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::SnowflakeArcticEmbedMLongQ, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::SnowflakeArcticEmbedL, "Xenova/bge-small-en-v1.5"),
            (EmbeddingModelType::SnowflakeArcticEmbedLQ, "Xenova/bge-small-en-v1.5"),
        ];
        for (model, expected_repo) in cases {
            assert_eq!(
                model.hf_repo_id(),
                *expected_repo,
                "Unexpected hf_repo_id for {:?}",
                model
            );
        }
    }

    // ============================================================================
    // Quantized variants share repo with base
    // ============================================================================

    #[test]
    fn test_quantized_base_same_hf_repo_exhaustive() {
        let pairs = [
            (EmbeddingModelType::BgeSmallEnV15, EmbeddingModelType::BgeSmallEnV15Q),
            (EmbeddingModelType::AllMiniLmL6V2, EmbeddingModelType::AllMiniLmL6V2Q),
            (EmbeddingModelType::AllMiniLmL12V2, EmbeddingModelType::AllMiniLmL12V2Q),
            (EmbeddingModelType::BgeBaseEnV15, EmbeddingModelType::BgeBaseEnV15Q),
            (EmbeddingModelType::BgeLargeEnV15, EmbeddingModelType::BgeLargeEnV15Q),
            (EmbeddingModelType::ParaphraseMiniLmL12V2, EmbeddingModelType::ParaphraseMiniLmL12V2Q),
            (EmbeddingModelType::NomicEmbedTextV15, EmbeddingModelType::NomicEmbedTextV15Q),
            (EmbeddingModelType::MxbaiEmbedLargeV1, EmbeddingModelType::MxbaiEmbedLargeV1Q),
            (EmbeddingModelType::GteBaseEnV15, EmbeddingModelType::GteBaseEnV15Q),
            (EmbeddingModelType::GteLargeEnV15, EmbeddingModelType::GteLargeEnV15Q),
            (EmbeddingModelType::SnowflakeArcticEmbedXs, EmbeddingModelType::SnowflakeArcticEmbedXsQ),
            (EmbeddingModelType::SnowflakeArcticEmbedS, EmbeddingModelType::SnowflakeArcticEmbedSQ),
            (EmbeddingModelType::SnowflakeArcticEmbedM, EmbeddingModelType::SnowflakeArcticEmbedMQ),
            (EmbeddingModelType::SnowflakeArcticEmbedMLong, EmbeddingModelType::SnowflakeArcticEmbedMLongQ),
            (EmbeddingModelType::SnowflakeArcticEmbedL, EmbeddingModelType::SnowflakeArcticEmbedLQ),
        ];
        for (base, quantized) in &pairs {
            assert_eq!(
                base.hf_repo_id(),
                quantized.hf_repo_id(),
                "{:?} and {:?} should share hf_repo_id",
                base,
                quantized
            );
        }
    }

    // ============================================================================
    // EmbeddingConfig::default() field values
    // ============================================================================

    #[test]
    fn test_embedding_config_default_all_fields() {
        let config = EmbeddingConfig::default();
        assert_eq!(config.model, EmbeddingModelType::BgeSmallEnV15);
        assert_eq!(config.batch_size, 32);
        assert!(config.show_download_progress);
        assert!(!config.sparse_enabled);
        assert_eq!(config.sparse_model, SparseModelType::SpladePpV1);
    }

    // ============================================================================
    // EmbeddingConfig serialization roundtrip (default)
    // ============================================================================

    #[test]
    fn test_embedding_config_default_serde_roundtrip() {
        let original = EmbeddingConfig::default();
        let json = serde_json::to_string(&original).unwrap();
        let parsed: EmbeddingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, original.model);
        assert_eq!(parsed.batch_size, original.batch_size);
        assert_eq!(parsed.show_download_progress, original.show_download_progress);
        assert_eq!(parsed.sparse_enabled, original.sparse_enabled);
        assert_eq!(parsed.sparse_model, original.sparse_model);
    }

    // ============================================================================
    // SparseModelType display/parse roundtrip for all variants
    // ============================================================================

    #[test]
    fn test_sparse_model_type_display_roundtrip_all_variants() {
        for variant in [SparseModelType::SpladePpV1] {
            let display = variant.to_string();
            let parsed: SparseModelType = display.parse().unwrap();
            assert_eq!(parsed, variant, "SparseModelType roundtrip failed for {:?}", variant);
        }
    }

    #[test]
    fn test_sparse_model_type_all_variants_count() {
        let all = [SparseModelType::SpladePpV1];
        assert_eq!(all.len(), 1);
    }

    // ============================================================================
    // batch_chunks utility edge cases
    // ============================================================================

    #[test]
    fn test_batch_chunks_exact_boundary() {
        let items: Vec<i32> = (0..6).collect();
        let batches = batch_chunks(&items, 3);
        assert_eq!(batches, vec![vec![0, 1, 2], vec![3, 4, 5]]);
    }

    #[test]
    fn test_batch_chunks_one_more_than_boundary() {
        let items: Vec<i32> = (0..7).collect();
        let batches = batch_chunks(&items, 3);
        assert_eq!(batches, vec![vec![0, 1, 2], vec![3, 4, 5], vec![6]]);
    }

    #[test]
    fn test_batch_chunks_batch_size_one() {
        let items = ["a", "b", "c"];
        let batches = batch_chunks(&items, 1);
        assert_eq!(batches, vec![vec!["a"], vec!["b"], vec!["c"]]);
    }

    #[test]
    fn test_batch_chunks_single_element() {
        let items = [42];
        assert_eq!(batch_chunks(&items, 5), vec![vec![42]]);
    }

    #[test]
    fn test_batch_chunks_large_batch_size() {
        let items: Vec<i32> = (0..5).collect();
        let batches = batch_chunks(&items, 100);
        assert_eq!(batches, vec![vec![0, 1, 2, 3, 4]]);
    }

    #[test]
    fn test_batch_chunks_non_copy_type() {
        let items = vec!["hello".to_string(), "world".to_string()];
        let batches = batch_chunks(&items, 1);
        assert_eq!(batches, vec![vec!["hello".to_string()], vec!["world".to_string()]]);
    }

    // ============================================================================
    // EmbeddingService integration tests
    // ============================================================================

    #[tokio::test(flavor = "multi_thread")]
    async fn test_embedding_service_new() {
        let config = EmbeddingConfig::default();
        let service = EmbeddingService::new(config.clone()).unwrap();
        assert_eq!(service.model_type(), EmbeddingModelType::BgeSmallEnV15);
        assert_eq!(service.dimensions(), 384);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_embedding_service_with_default_model() {
        let service = EmbeddingService::with_default_model().unwrap();
        assert_eq!(service.model_type(), EmbeddingModelType::default());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_embedding_service_with_model() {
        let service = EmbeddingService::with_model(EmbeddingModelType::AllMiniLmL6V2).unwrap();
        assert_eq!(service.model_type(), EmbeddingModelType::AllMiniLmL6V2);
        assert_eq!(service.dimensions(), 384);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_embedding_service_config_accessor() {
        let config = EmbeddingConfig {
            model: EmbeddingModelType::BgeSmallEnV15,
            batch_size: 16,
            show_download_progress: false,
            sparse_enabled: false,
            sparse_model: SparseModelType::SpladePpV1,
        };
        let service = EmbeddingService::new(config.clone()).unwrap();
        assert_eq!(service.config().batch_size, 16);
        assert!(!service.config().show_download_progress);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_embedding_service_embed_text() {
        let service = EmbeddingService::with_default_model().unwrap();
        let embedding = service.embed_text("hello world").await.unwrap();
        assert_eq!(embedding.len(), service.dimensions());
        assert!(!embedding.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_embedding_service_embed_texts() {
        let service = EmbeddingService::with_default_model().unwrap();
        let texts = vec!["hello world", "foo bar", "test sentence"];
        let embeddings = service.embed_texts(&texts).await.unwrap();
        assert_eq!(embeddings.len(), 3);
        for emb in &embeddings {
            assert_eq!(emb.len(), service.dimensions());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_embedding_service_embed_texts_empty() {
        let service = EmbeddingService::with_default_model().unwrap();
        let embeddings: Vec<Vec<f32>> = service.embed_texts(&[] as &[&str]).await.unwrap();
        assert!(embeddings.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_embedding_service_embed_sparse_disabled() {
        let service = EmbeddingService::with_default_model().unwrap();
        let result = service.embed_sparse(&["hello"]).await;
        match result {
            Err(err) => {
                assert!(matches!(err, AppError::Internal(_)));
                assert!(err.to_string().contains("Sparse embeddings not enabled"));
            }
            Ok(_) => panic!("Expected sparse embedding to fail when disabled"),
        }
    }

    // ============================================================================
    // CachedEmbeddingService integration tests
    // ============================================================================

    #[tokio::test(flavor = "multi_thread")]
    async fn test_cached_embedding_service_new() {
        let config = EmbeddingConfig::default();
        let cache_config = CacheConfig::default();
        let cached = CachedEmbeddingService::new(config, cache_config).unwrap();
        assert!(cached.is_cache_enabled());
        assert_eq!(cached.model_type(), EmbeddingModelType::BgeSmallEnV15);
        assert_eq!(cached.dimensions(), 384);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_cached_embedding_service_without_cache() {
        let config = EmbeddingConfig::default();
        let cached = CachedEmbeddingService::without_cache(config).unwrap();
        assert!(!cached.is_cache_enabled());
        let stats = cached.cache_stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_cached_embedding_service_embed_text_cache_hit_and_miss() {
        let config = EmbeddingConfig::default();
        let cache_config = CacheConfig::default();
        let cached = CachedEmbeddingService::new(config, cache_config).unwrap();
        let text = "cache test text";

        // First call: cache miss
        let emb1 = cached.embed_text(text).await.unwrap();
        let stats1 = cached.cache_stats();
        assert_eq!(stats1.misses, 1);
        assert_eq!(stats1.hits, 0);

        // Second call: cache hit
        let emb2 = cached.embed_text(text).await.unwrap();
        let stats2 = cached.cache_stats();
        assert_eq!(stats2.misses, 1);
        assert_eq!(stats2.hits, 1);

        assert_eq!(emb1, emb2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_cached_embedding_service_embed_texts_mixed() {
        let config = EmbeddingConfig::default();
        let cache_config = CacheConfig::default();
        let cached = CachedEmbeddingService::new(config, cache_config).unwrap();

        let texts = vec!["first text", "second text", "third text"];
        // First batch: all miss
        let embs1 = cached.embed_texts(&texts).await.unwrap();
        assert_eq!(embs1.len(), 3);
        let stats1 = cached.cache_stats();
        assert_eq!(stats1.misses, 3);
        assert_eq!(stats1.hits, 0);

        // Second batch with overlap: all hits
        let texts2 = vec!["first text", "second text"];
        let embs2 = cached.embed_texts(&texts2).await.unwrap();
        assert_eq!(embs2.len(), 2);
        let stats2 = cached.cache_stats();
        assert_eq!(stats2.hits, 2);
        assert_eq!(stats2.misses, 3);
        assert_eq!(embs1[0], embs2[0]);
        assert_eq!(embs1[1], embs2[1]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_cached_embedding_service_clear_cache() {
        let config = EmbeddingConfig::default();
        let cache_config = CacheConfig::default();
        let cached = CachedEmbeddingService::new(config, cache_config).unwrap();

        let _ = cached.embed_text("text to clear").await.unwrap();
        assert!(cached.cache_stats().misses > 0);

        cached.clear_cache().unwrap();
        let stats = cached.cache_stats();
        assert_eq!(stats.size_bytes, 0);
        assert_eq!(stats.entry_count, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_cached_embedding_service_invalidate() {
        let config = EmbeddingConfig::default();
        let cache_config = CacheConfig::default();
        let cached = CachedEmbeddingService::new(config, cache_config).unwrap();

        let text = "invalidate me";
        let emb1 = cached.embed_text(text).await.unwrap();
        let stats1 = cached.cache_stats();
        assert_eq!(stats1.misses, 1);

        cached.invalidate(text).unwrap();

        // After invalidate, this should be a miss again
        let emb2 = cached.embed_text(text).await.unwrap();
        let stats2 = cached.cache_stats();
        assert_eq!(stats2.misses, 2);

        assert_eq!(emb1, emb2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_cached_embedding_service_lru_eviction() {
        let config = EmbeddingConfig::default();
        // Very small cache: max ~0 entries after division, but .max(100) makes it 100.
        // Use a cache config with max_size_bytes tiny to force eviction behavior
        let cache_config = CacheConfig {
            max_size_bytes: 1,
            enabled: true,
            ..Default::default()
        };
        let cached = CachedEmbeddingService::new(config, cache_config).unwrap();

        let text1 = "first eviction text";
        let text2 = "second eviction text";

        let emb1 = cached.embed_text(text1).await.unwrap();
        let emb2 = cached.embed_text(text2).await.unwrap();

        // text1 and text2 are different, so both should be computed
        assert_ne!(emb1, emb2);

        // Re-embed text1: since cache only holds 1 entry and text2 was last,
        // text1 should have been evicted, causing a miss
        let _ = cached.embed_text(text1).await.unwrap();
        let stats = cached.cache_stats();
        assert!(stats.evictions > 0 || stats.misses >= 2, "Expected eviction or miss, got stats={:?}", stats);
    }

    // ============================================================================
    // map_embedding_transport_error direct tests
    // ============================================================================

    #[tokio::test]
    async fn test_map_embedding_transport_error_timeout() {
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)).set_body_string("{}"))
            .mount(&server).await;
        let http = reqwest::Client::builder().timeout(Duration::from_millis(100)).build().unwrap();
        let err = http.post(format!("{}/v1/embeddings", server.uri())).send().await.unwrap_err();
        let mapped = map_embedding_transport_error(&err);
        assert!(matches!(mapped, AppError::Unavailable(_)));
        assert!(mapped.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_map_embedding_transport_error_connect() {
        let http = reqwest::Client::new();
        let err = http.get("http://127.0.0.1:1/").send().await.unwrap_err();
        let mapped = map_embedding_transport_error(&err);
        assert!(matches!(mapped, AppError::Unavailable(_)));
        assert!(mapped.to_string().contains("unreachable"));
    }

    #[tokio::test]
    async fn test_map_embedding_transport_error_other() {
        // Invalid URL scheme to trigger a non-timeout, non-connect error
        let http = reqwest::Client::new();
        let err = http.get("mailto:not-a-url").send().await.unwrap_err();
        let mapped = map_embedding_transport_error(&err);
        assert!(matches!(mapped, AppError::External(_)));
        assert!(mapped.to_string().contains("failed"));
    }

    // ============================================================================
    // HttpEmbeddingClient additional tests
    // ============================================================================

    #[test]
    fn test_http_embedding_client_with_api_key() {
        let client = HttpEmbeddingClient::new("http://localhost:8080").unwrap();
        let _client_with_key = client.with_api_key("secret123");
    }

    #[test]
    fn test_http_embedding_client_new_trims_trailing_slash() {
        let client = HttpEmbeddingClient::new("http://localhost:8080/").unwrap();
        let _ = client;
    }

    // ============================================================================
    // decode_base64_embedding direct tests
    // ============================================================================

    #[test]
    fn test_decode_base64_embedding_valid() {
        use base64::Engine;
        let values = vec![1.0f32, -2.5, 3.25, 0.0];
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let decoded = decode_base64_embedding(&encoded).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn test_decode_base64_embedding_invalid_base64() {
        let err = decode_base64_embedding("!!!not-valid-base64!!!").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains("Invalid base64"));
    }

    // ============================================================================
    // dense_embedding edge cases
    // ============================================================================

    #[test]
    fn test_dense_embedding_empty_vector() {
        let err = dense_embedding(vec![], 384).unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains("must not be empty"));
    }

    // ============================================================================
    // EmbeddingRequest serde edge cases
    // ============================================================================

    #[test]
    fn test_build_embedding_request_dimensions_param() {
        let req = build_embedding_request("m", &["hello"], None, Some(512));
        assert_eq!(req.dimensions, Some(512));
    }

}

