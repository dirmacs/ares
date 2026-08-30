//! Reranking for improving search result relevance.
//!
//! This module provides reranking capabilities using cross-encoder models
//! to improve the quality of retrieved documents after initial retrieval.
//!
//! # Feature Flag
//!
//! This module requires the `local-embeddings` feature to be enabled.
//! Without it, local ONNX-based reranking is not available.
//!
//! ```toml
//! [dependencies]
//! ares-server = { version = "0.3", features = ["local-embeddings"] }
//! ```

use std::cmp::Ordering;
use std::str::FromStr;
use std::sync::Arc;

use fastembed::{RerankInitOptions, RerankerModel as FastEmbedRerankerModel, TextRerank};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use ares_types::types::{AppError, Result};

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

    /// Get the HuggingFace repo ID for this model (used for lancor pre-downloading)
    pub fn hf_repo_id(&self) -> &'static str {
        match self {
            Self::BgeRerankerBase => "BAAI/bge-reranker-base",
            Self::BgeRerankerV2M3 => "BAAI/bge-reranker-v2-m3",
            Self::JinaRerankerV1TurboEn => "jinaai/jina-reranker-v1-turbo-en",
            Self::JinaRerankerV2BaseMultilingual => "jinaai/jina-reranker-v2-base-multilingual",
        }
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

pub use crate::rerank_types::RerankedResult;

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
                    // Pre-download ONNX model files via lancor to bypass hf-hub/ureq xethub bug
                    let repo_id = config.model.hf_repo_id();
                    let onnx_files = &["onnx/model.onnx", "tokenizer.json", "config.json"];
                    let cache_dir = std::env::var("FASTEMBED_CACHE_DIR")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|_| {
                            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                            std::path::PathBuf::from(home).join(".cache").join("fastembed")
                        });
                    if let Err(e) = super::embeddings::pre_download_model(repo_id, onnx_files, &cache_dir) {
                        tracing::warn!("Reranker pre-download failed (may already be cached): {}", e);
                    }

                    let init_options = RerankInitOptions::new(config.model.to_fastembed_model())
                        .with_show_download_progress(config.show_download_progress);
                    let model = TextRerank::try_new(init_options).map_err(|e| {
                        AppError::Internal(format!("Failed to load reranker: {}", e))
                    })?;
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
        let documents: Vec<String> = results
            .iter()
            .map(|(_, content, _)| content.clone())
            .collect();

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
        let documents: Vec<String> = results
            .iter()
            .map(|(_, content, _)| content.clone())
            .collect();

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

    /// Mirrors retrieval-score normalization in `rerank_hybrid`.
    fn normalize_retrieval_score(score: f32, min_retrieval: f32, max_retrieval: f32) -> f32 {
        let retrieval_range = max_retrieval - min_retrieval;
        if retrieval_range > 0.0 {
            (score - min_retrieval) / retrieval_range
        } else {
            1.0
        }
    }

    /// Mirrors hybrid final-score calculation in `rerank_hybrid`.
    fn compute_hybrid_final_score(
        normalized_retrieval: f32,
        rerank_score: f32,
        rerank_weight: f32,
    ) -> f32 {
        let retrieval_weight = 1.0 - rerank_weight;
        retrieval_weight * normalized_retrieval + rerank_weight * rerank_score
    }

    /// Mirrors sort → assign ranks → truncate used by `rerank` / `rerank_hybrid`.
    fn apply_rerank_ranking(mut results: Vec<RerankedResult>, top_k: usize) -> Vec<RerankedResult> {
        results.sort_by(|a, b| {
            b.final_score
                .partial_cmp(&a.final_score)
                .unwrap_or(Ordering::Equal)
        });
        for (idx, result) in results.iter_mut().enumerate() {
            result.new_rank = idx + 1;
        }
        results.truncate(top_k);
        results
    }

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

    #[tokio::test]
    async fn test_rerank_hybrid_empty() {
        let reranker = Reranker::default_reranker();
        let results = reranker
            .rerank_hybrid("test query", &[], 0.5, None)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_display_roundtrip_all_reranker_models() {
        for model in RerankerModelType::all() {
            let display = model.to_string();
            let parsed: RerankerModelType = display.parse().unwrap_or_else(|_| {
                panic!("Display→FromStr roundtrip failed for {:?} ('{}')", model, display)
            });
            assert_eq!(parsed, model);
        }
    }

    #[test]
    fn test_reranker_from_str_aliases() {
        let aliases = vec![
            ("bge-base", RerankerModelType::BgeRerankerBase),
            ("bge-m3", RerankerModelType::BgeRerankerV2M3),
            ("jina-turbo", RerankerModelType::JinaRerankerV1TurboEn),
            ("jina-multilingual", RerankerModelType::JinaRerankerV2BaseMultilingual),
        ];
        for (alias, expected) in aliases {
            let parsed: RerankerModelType = alias.parse().unwrap();
            assert_eq!(parsed, expected, "Alias '{}' mismatch", alias);
        }
    }

    #[test]
    fn test_reranker_from_str_case_insensitive() {
        let parsed: RerankerModelType = "BGE-RERANKER-BASE".parse().unwrap();
        assert_eq!(parsed, RerankerModelType::BgeRerankerBase);
    }

    #[test]
    fn test_reranker_from_str_invalid() {
        let result = "fake-reranker".parse::<RerankerModelType>();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Unknown reranker model"));
    }

    #[test]
    fn test_hf_repo_id_all_models() {
        for model in RerankerModelType::all() {
            let repo = model.hf_repo_id();
            assert!(!repo.is_empty(), "{:?} has empty repo ID", model);
            assert!(repo.contains('/'), "{:?} repo '{}' should have org/model format", model, repo);
        }
    }

    #[test]
    fn test_reranker_config_serialization() {
        let config = RerankerConfig {
            model: RerankerModelType::JinaRerankerV1TurboEn,
            show_download_progress: false,
            top_k: 5,
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"model\":\"jina-reranker-v1-turbo-en\""));
        assert!(json.contains("\"top_k\":5"));
        assert!(json.contains("\"show_download_progress\":false"));

        let parsed: RerankerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, RerankerModelType::JinaRerankerV1TurboEn);
        assert_eq!(parsed.top_k, 5);
        assert!(!parsed.show_download_progress);
    }

    #[test]
    fn test_reranker_config_deserialize_defaults() {
        let parsed: RerankerConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.model, RerankerModelType::BgeRerankerBase);
        assert_eq!(parsed.top_k, 10);
        assert!(parsed.show_download_progress);
    }

    #[test]
    fn test_reranker_config_deserialize_partial() {
        let json = r#"{"model":"bge-reranker-v2-m3","top_k":3}"#;
        let parsed: RerankerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.model, RerankerModelType::BgeRerankerV2M3);
        assert_eq!(parsed.top_k, 3);
        assert!(parsed.show_download_progress);
    }

    #[test]
    fn test_reranker_model_type_serde_kebab_case() {
        let json = serde_json::to_string(&RerankerModelType::JinaRerankerV2BaseMultilingual).unwrap();
        assert_eq!(json, "\"jina-reranker-v2-base-multilingual\"");

        let parsed: RerankerModelType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, RerankerModelType::JinaRerankerV2BaseMultilingual);
    }

    #[test]
    fn test_reranked_result_serialization() {
        let result = RerankedResult {
            id: "doc-1".to_string(),
            content: "test content".to_string(),
            retrieval_score: 0.8,
            rerank_score: 0.95,
            final_score: 0.9,
            original_rank: 3,
            new_rank: 1,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"id\":\"doc-1\""));
        assert!(json.contains("\"new_rank\":1"));

        let parsed: RerankedResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "doc-1");
        assert_eq!(parsed.original_rank, 3);
        assert_eq!(parsed.new_rank, 1);
    }

    #[test]
    fn test_to_fastembed_reranker_all_variants() {
        for model in RerankerModelType::all() {
            let _ = model.to_fastembed_model(); // should not panic
        }
    }

    #[test]
    fn test_reranker_new_and_model_type() {
        let config = RerankerConfig {
            model: RerankerModelType::BgeRerankerV2M3,
            ..Default::default()
        };
        let reranker = Reranker::new(config);
        assert_eq!(reranker.model_type(), RerankerModelType::BgeRerankerV2M3);
    }
    #[test]
    fn test_reranked_result_score_ordering() {
        let mut results = vec![
            RerankedResult {
                id: "a".into(),
                content: "a".into(),
                retrieval_score: 0.5,
                rerank_score: 0.2,
                final_score: 0.3,
                original_rank: 2,
                new_rank: 0,
            },
            RerankedResult {
                id: "b".into(),
                content: "b".into(),
                retrieval_score: 0.4,
                rerank_score: 0.9,
                final_score: 0.9,
                original_rank: 1,
                new_rank: 0,
            },
        ];
        results.sort_by(|a, b| {
            b.final_score
                .partial_cmp(&a.final_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        assert_eq!(results[0].id, "b");
        assert_eq!(results[1].id, "a");
    }

    #[test]
    fn test_hybrid_score_formula_known_inputs() {
        let rerank_weight = 0.6_f32;
        let retrieval_score = 0.8_f32;
        let rerank_score = 0.5_f32;
        let min_r = 0.2_f32;
        let max_r = 0.8_f32;
        let normalized = normalize_retrieval_score(retrieval_score, min_r, max_r);
        let final_score = compute_hybrid_final_score(normalized, rerank_score, rerank_weight);
        assert!((final_score - 0.7).abs() < 0.001);
    }

    #[test]
    fn test_retrieval_normalization_spreads_scores() {
        assert!((normalize_retrieval_score(0.2, 0.2, 0.8) - 0.0).abs() < f32::EPSILON);
        assert!((normalize_retrieval_score(0.5, 0.2, 0.8) - 0.5).abs() < f32::EPSILON);
        assert!((normalize_retrieval_score(0.8, 0.2, 0.8) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_retrieval_normalization_equal_scores() {
        assert!((normalize_retrieval_score(0.42, 0.5, 0.5) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_hybrid_score_pure_rerank_weight() {
        let final_score = compute_hybrid_final_score(0.0, 0.85, 1.0);
        assert!((final_score - 0.85).abs() < f32::EPSILON);
    }

    #[test]
    fn test_hybrid_score_pure_retrieval_weight() {
        let final_score = compute_hybrid_final_score(0.75, 0.99, 0.0);
        assert!((final_score - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn test_standard_rerank_final_score_equals_rerank_score() {
        let rerank_scores = [0.1_f32, 0.9, 0.5];
        let results: Vec<RerankedResult> = rerank_scores
            .iter()
            .enumerate()
            .map(|(idx, &rerank_score)| RerankedResult {
                id: format!("doc-{idx}"),
                content: format!("content-{idx}"),
                retrieval_score: 1.0 - rerank_score,
                rerank_score,
                final_score: rerank_score,
                original_rank: idx + 1,
                new_rank: 0,
            })
            .collect();

        let ranked = apply_rerank_ranking(results, 2);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].id, "doc-1");
        assert_eq!(ranked[0].new_rank, 1);
        assert_eq!(ranked[0].final_score, ranked[0].rerank_score);
        assert_eq!(ranked[1].id, "doc-2");
        assert_eq!(ranked[1].new_rank, 2);
    }

    #[test]
    fn test_hybrid_rerank_algorithm_sort_and_truncate() {
        let min_r = 0.1_f32;
        let max_r = 0.9_f32;
        let rerank_weight = 0.7_f32;
        let candidates = [
            ("low", 0.9_f32, 0.1_f32),
            ("mid", 0.5_f32, 0.5_f32),
            ("high", 0.2_f32, 0.95_f32),
        ];

        let results: Vec<RerankedResult> = candidates
            .iter()
            .enumerate()
            .map(|(idx, (id, retrieval_score, rerank_score))| {
                let normalized =
                    normalize_retrieval_score(*retrieval_score, min_r, max_r);
                let final_score =
                    compute_hybrid_final_score(normalized, *rerank_score, rerank_weight);
                RerankedResult {
                    id: (*id).to_string(),
                    content: (*id).to_string(),
                    retrieval_score: *retrieval_score,
                    rerank_score: *rerank_score,
                    final_score,
                    original_rank: idx + 1,
                    new_rank: 0,
                }
            })
            .collect();

        let ranked = apply_rerank_ranking(results, 2);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].id, "high");
        assert_eq!(ranked[0].new_rank, 1);
        assert_eq!(ranked[1].id, "mid");
        assert_eq!(ranked[1].new_rank, 2);
        assert!(ranked[0].final_score > ranked[1].final_score);
    }

    #[test]
    fn test_rerank_algorithm_preserves_stable_order_on_tie() {
        let results = vec![
            RerankedResult {
                id: "first".into(),
                content: "a".into(),
                retrieval_score: 0.5,
                rerank_score: 0.5,
                final_score: 0.5,
                original_rank: 1,
                new_rank: 0,
            },
            RerankedResult {
                id: "second".into(),
                content: "b".into(),
                retrieval_score: 0.5,
                rerank_score: 0.5,
                final_score: 0.5,
                original_rank: 2,
                new_rank: 0,
            },
        ];
        let ranked = apply_rerank_ranking(results, 2);
        assert_eq!(ranked[0].id, "first");
        assert_eq!(ranked[1].id, "second");
    }

}
