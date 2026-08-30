//! Shared rerank result type.
//!
//! Always compiled so both the ONNX reranker (`local-embeddings`) and
//! [`crate::llm_rerank`] can return the same struct.

use serde::{Deserialize, Serialize};

/// A reranked search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankedResult {
    /// Document ID
    pub id: String,
    /// Document content
    pub content: String,
    /// Original retrieval score
    pub retrieval_score: f32,
    /// Reranking score from cross-encoder
    pub rerank_score: f32,
    /// Final combined score (used for ranking)
    pub final_score: f32,
    /// Original rank before reranking
    pub original_rank: usize,
    /// New rank after reranking
    pub new_rank: usize,
}
