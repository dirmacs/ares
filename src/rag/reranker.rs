//! Reranking for improving search result relevance.
//!
//! This module provides reranking capabilities using cross-encoder models
//! to improve the quality of retrieved documents after initial retrieval.

use std::cmp::Ordering;
use std::str::FromStr;
use std::sync::Arc;

use fastembed::{RerankerModel as FastEmbedRerankerModel, RerankInitOptions, TextRerank};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use crate::types::{AppError, Result};

// ============================================================================
// Reranker Model Types
// ============================================================================

/// Supported reranking models
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RerankerModelType {
    /// BGE Reranker Base - English/Chinese, good balance of speed and quality
    #[default]
    BgeRerankerBase,
    /// BGE Reranker v2 M3 - Multilingual reranker
    BgeRerankerV2M3,
    /// Jina Reranker v1 Turbo - Fast English reranker
    JinaRerankerV1TurboEn,
    /// Jina Reranker v2 Base - Multilingual reranker
    JinaRerankerV2BaseMultilingual,
}

impl RerankerModelType {
    /// Convert to fastembed's RerankerModel enum
    pub fn to_fastembed_model(&self) -> FastEmbedRerankerModel {
        match self {
            Self::BgeRerankerBase => FastEmbedRerankerModel::BGERerankerBase,
            Self::BgeRerankerV2M3 => FastEmbedRerankerModel::BGERerankerV2M3,
            Self::JinaRerankerV1TurboEn => FastEmbedRerankerModel::JINARerankerV1TurboEn,
            // Note: typo in fastembed - "Multiligual" instead of "Multilingual"
            Self::JinaRerankerV2BaseMultilingual => {
                FastEmbedRerankerModel::JINARerankerV2BaseMultiligual
            }
        }
    }

    /// Get all available models
    pub fn all() -> Vec<Self> {
        vec![
            Self::BgeRerankerBase,
            Self::BgeRerankerV2M3,
            Self::JinaRerankerV1TurboEn,
            Self::JinaRerankerV2BaseMultilingual,
        ]
    }

    /// Check if this model is multilingual
    pub fn is_multilingual(&self) -> bool {
        matches!(
            self,
            Self::JinaRerankerV2BaseMultilingual | Self::BgeRerankerV2M3
        )
    }
}

impl FromStr for RerankerModelType {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "bge-reranker-base" | "bge-base" => Ok(Self::BgeRerankerBase),
            "bge-reranker-v2-m3" | "bge-m3" => Ok(Self::BgeRerankerV2M3),
            "jina-reranker-v1-turbo-en" | "jina-turbo" => Ok(Self::JinaRerankerV1TurboEn),
            "jina-reranker-v2-base-multilingual" | "jina-multilingual" => {
                Ok(Self::JinaRerankerV2BaseMultilingual)
            }
            _ => Err(AppError::Internal(format!(
                "Unknown reranker model: {}. Use one of: bge-reranker-base, \
                 bge-reranker-v2-m3, jina-reranker-v1-turbo-en, jina-reranker-v2-base-multilingual",
                s
            ))),
        }
    }
}

impl std::fmt::Display for RerankerModelType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::BgeRerankerBase => "bge-reranker-base",
            Self::BgeRerankerV2M3 => "bge-reranker-v2-m3",
            Self::JinaRerankerV1TurboEn => "jina-reranker-v1-turbo-en",
            Self::JinaRerankerV2BaseMultilingual => "jina-reranker-v2-base-multilingual",
        };
        write!(f, "{}", name)
    }
}

// ============================================================================
// Reranker Configuration
// ============================================================================

/// Configuration for the reranking service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankerConfig {
    /// Model to use for reranking
    #[serde(default)]
    pub model: RerankerModelType,
    /// Show download progress when fetching model weights
    #[serde(default = "default_show_progress")]
    pub show_download_progress: bool,
    /// Number of top results to return after reranking
    #[serde(default = "default_top_k")]
    pub top_k: usize,
}

fn default_show_progress() -> bool {
    true
}

fn default_top_k() -> usize {
    10
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            model: RerankerModelType::default(),
            show_download_progress: default_show_progress(),
            top_k: default_top_k(),
        }
    }
}

// ============================================================================
// Reranked Result
// ============================================================================

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

// ============================================================================
// Reranker Service
// ============================================================================

/// Reranking service using cross-encoder models
pub struct Reranker {
    config: RerankerConfig,
    model: OnceCell<Arc<tokio::sync::Mutex<TextRerank>>>,
}

impl Reranker {
    /// Create a new reranker with the given configuration
    pub fn new(config: RerankerConfig) -> Self {
        Self {
            config,
            model: OnceCell::new(),
        }
    }

    /// Create with default configuration
    pub fn default_reranker() -> Self {
        Self::new(RerankerConfig::default())
    }

    /// Get or initialize the reranking model
    async fn get_model(&self) -> Result<Arc<tokio::sync::Mutex<TextRerank>>> {
        self.model
            .get_or_try_init(|| async {
                let config = self.config.clone();
                tokio::task::spawn_blocking(move || {
                    let init_options = RerankInitOptions::new(config.model.to_fastembed_model())
                        .with_show_download_progress(config.show_download_progress);
                    let model = TextRerank::try_new(init_options)
                        .map_err(|e| AppError::Internal(format!("Failed to load reranker: {}", e)))?;
                    Ok(Arc::new(tokio::sync::Mutex::new(model)))
                })
                .await
                .map_err(|e| AppError::Internal(format!("Reranker task failed: {}", e)))?
            })
            .await
            .map(Arc::clone)
    }

    /// Rerank search results
    ///
    /// Takes a query and a list of (id, content, score) tuples and returns
    /// reranked results sorted by relevance.
    pub async fn rerank(
        &self,
        query: &str,
        results: &[(String, String, f32)],
        top_k: Option<usize>,
    ) -> Result<Vec<RerankedResult>> {
        if results.is_empty() {
            return Ok(Vec::new());
        }

        let model = self.get_model().await?;
        let documents: Vec<String> = results.iter().map(|(_, content, _)| content.clone()).collect();

        let query = query.to_string();
        let rerank_scores = tokio::task::spawn_blocking(move || {
            let mut model = model.blocking_lock();
            model.rerank(query, &documents, true, None)
        })
        .await
        .map_err(|e| AppError::Internal(format!("Rerank task failed: {}", e)))?
        .map_err(|e| AppError::Internal(format!("Reranking failed: {}", e)))?;

        // Combine with original results
        let mut reranked: Vec<RerankedResult> = results
            .iter()
            .enumerate()
            .map(|(idx, (id, content, retrieval_score))| {
                let rerank_score = rerank_scores
                    .iter()
                    .find(|r| r.index == idx)
                    .map(|r| r.score)
                    .unwrap_or(0.0);

                RerankedResult {
                    id: id.clone(),
                    content: content.clone(),
                    retrieval_score: *retrieval_score,
                    rerank_score,
                    // Use rerank score as final score (could be combined differently)
                    final_score: rerank_score,
                    original_rank: idx + 1,
                    new_rank: 0, // Will be set after sorting
                }
            })
            .collect();

        // Sort by rerank score (higher is better)
        reranked.sort_by(|a, b| {
            b.final_score
                .partial_cmp(&a.final_score)
                .unwrap_or(Ordering::Equal)
        });

        // Assign new ranks
        for (idx, result) in reranked.iter_mut().enumerate() {
            result.new_rank = idx + 1;
        }

        // Truncate to top_k
        let top_k = top_k.unwrap_or(self.config.top_k);
        reranked.truncate(top_k);

        Ok(reranked)
    }

    /// Rerank with hybrid scoring
    ///
    /// Combines retrieval score with rerank score using a configurable weight
    pub async fn rerank_hybrid(
        &self,
        query: &str,
        results: &[(String, String, f32)],
        rerank_weight: f32,
        top_k: Option<usize>,
    ) -> Result<Vec<RerankedResult>> {
        if results.is_empty() {
            return Ok(Vec::new());
        }

        let model = self.get_model().await?;
        let documents: Vec<String> = results.iter().map(|(_, content, _)| content.clone()).collect();

        let query = query.to_string();
        let rerank_scores = tokio::task::spawn_blocking(move || {
            let mut model = model.blocking_lock();
            model.rerank(query, &documents, true, None)
        })
        .await
        .map_err(|e| AppError::Internal(format!("Rerank task failed: {}", e)))?
        .map_err(|e| AppError::Internal(format!("Reranking failed: {}", e)))?;

        // Normalize retrieval scores to 0-1 range
        let max_retrieval = results
            .iter()
            .map(|(_, _, s)| *s)
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal))
            .unwrap_or(1.0);
        let min_retrieval = results
            .iter()
            .map(|(_, _, s)| *s)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal))
            .unwrap_or(0.0);
        let retrieval_range = max_retrieval - min_retrieval;

        // Combine with original results
        let retrieval_weight = 1.0 - rerank_weight;
        let mut reranked: Vec<RerankedResult> = results
            .iter()
            .enumerate()
            .map(|(idx, (id, content, retrieval_score))| {
                let rerank_score = rerank_scores
                    .iter()
                    .find(|r| r.index == idx)
                    .map(|r| r.score)
                    .unwrap_or(0.0);

                // Normalize retrieval score
                let normalized_retrieval = if retrieval_range > 0.0 {
                    (retrieval_score - min_retrieval) / retrieval_range
                } else {
                    1.0
                };

                // Compute hybrid score
                let final_score =
                    retrieval_weight * normalized_retrieval + rerank_weight * rerank_score;

                RerankedResult {
                    id: id.clone(),
                    content: content.clone(),
                    retrieval_score: *retrieval_score,
                    rerank_score,
                    final_score,
                    original_rank: idx + 1,
                    new_rank: 0,
                }
            })
            .collect();

        // Sort by final score (higher is better)
        reranked.sort_by(|a, b| {
            b.final_score
                .partial_cmp(&a.final_score)
                .unwrap_or(Ordering::Equal)
        });

        // Assign new ranks
        for (idx, result) in reranked.iter_mut().enumerate() {
            result.new_rank = idx + 1;
        }

        // Truncate to top_k
        let top_k = top_k.unwrap_or(self.config.top_k);
        reranked.truncate(top_k);

        Ok(reranked)
    }

    /// Get the model type
    pub fn model_type(&self) -> RerankerModelType {
        self.config.model
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reranker_model_from_str() {
        assert_eq!(
            "bge-reranker-base".parse::<RerankerModelType>().unwrap(),
            RerankerModelType::BgeRerankerBase
        );
        assert_eq!(
            "bge-m3".parse::<RerankerModelType>().unwrap(),
            RerankerModelType::BgeRerankerV2M3
        );
        assert_eq!(
            "jina-multilingual".parse::<RerankerModelType>().unwrap(),
            RerankerModelType::JinaRerankerV2BaseMultilingual
        );
    }

    #[test]
    fn test_reranker_model_display() {
        assert_eq!(
            RerankerModelType::BgeRerankerBase.to_string(),
            "bge-reranker-base"
        );
        assert_eq!(
            RerankerModelType::JinaRerankerV2BaseMultilingual.to_string(),
            "jina-reranker-v2-base-multilingual"
        );
    }

    #[test]
    fn test_reranker_model_multilingual() {
        assert!(!RerankerModelType::BgeRerankerBase.is_multilingual());
        assert!(RerankerModelType::JinaRerankerV2BaseMultilingual.is_multilingual());
        assert!(RerankerModelType::BgeRerankerV2M3.is_multilingual());
    }

    #[test]
    fn test_all_models() {
        let all = RerankerModelType::all();
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn test_default_config() {
        let config = RerankerConfig::default();
        assert_eq!(config.model, RerankerModelType::BgeRerankerBase);
        assert_eq!(config.top_k, 10);
        assert!(config.show_download_progress);
    }

    #[tokio::test]
    async fn test_rerank_empty() {
        let reranker = Reranker::default_reranker();
        let results = reranker.rerank("test query", &[], None).await.unwrap();
        assert!(results.is_empty());
    }
}
