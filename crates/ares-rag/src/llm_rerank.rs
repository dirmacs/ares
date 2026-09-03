//! Remote reranking via the Cordis [`ares_llm::Llm`] service.
//!
//! Always compiled (not gated on `local-embeddings`). Local ONNX cross-encoders
//! stay in [`crate::reranker`]; this module is the remote path that does not
//! pull fastembed.
//!
//! genai is a private HTTP adapter inside `ares-llm`. This crate never imports
//! `genai` or `GenaiClient`. Callers look up the `Llm` service and go through
//! [`ares_llm::Llm::complete`], which owns client resolution, intercepts, and
//! the `llm.complete` waterfall.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use ares_types::{AppError, Result};
use cordis::Context;
use serde::Deserialize;

use crate::rerank_types::RerankedResult;

#[derive(Debug, Deserialize)]
struct LlmScoreEntry {
    id: String,
    score: f32,
}

#[derive(Debug, Deserialize)]
struct LlmScoreResponse {
    scores: Vec<LlmScoreEntry>,
}

/// Rerank `results` through [`ares_llm::Llm`] when that service is on `ctx`.
///
/// Looks up `Llm` with `ctx.get::<Llm>()`, then `llm.complete(&ctx, prompt)`.
/// The prompt asks for JSON `{"scores":[{"id":"...","score":0.0}]}`. Scores
/// are joined back to the input rows, sorted descending, ranked, and truncated
/// to `top_k`.
///
/// Empty `results` returns `Ok(vec![])` without looking up `Llm`.
///
/// # Errors
///
/// Returns [`AppError::Configuration`] if `Llm` is not provided on `ctx`.
/// Other errors come from `Llm::complete` or from parsing the JSON scores.
pub async fn rerank_with_llm(
    ctx: &Arc<Context>,
    query: &str,
    results: &[(String, String, f32)],
    top_k: usize,
) -> Result<Vec<RerankedResult>> {
    if results.is_empty() {
        return Ok(Vec::new());
    }
    let Some(llm) = ctx.get::<ares_llm::Llm>() else {
        return Err(AppError::Configuration(
            "Llm service is not provided for remote rerank".into(),
        ));
    };
    let prompt = build_rerank_prompt(query, results);
    let raw = llm.complete(ctx, &prompt).await?;
    apply_llm_scores(results, &raw, top_k)
}

fn build_rerank_prompt(query: &str, results: &[(String, String, f32)]) -> String {
    let mut prompt = String::from(
        "Score each document for relevance to the query. \
         Return ONLY JSON of the form {\"scores\":[{\"id\":\"...\",\"score\":0.0}]} \
         with one object per document id. score is a number from 0 to 1 \
         (1 is most relevant).\n\nQuery: ",
    );
    prompt.push_str(query);
    prompt.push_str("\n\nDocuments:\n");
    for (id, content, _) in results {
        prompt.push_str("[id=");
        prompt.push_str(id);
        prompt.push_str("]\n");
        prompt.push_str(content);
        prompt.push('\n');
    }
    prompt
}

fn json_object_slice(text: &str) -> Result<&str> {
    let start = text.find('{').ok_or_else(|| {
        AppError::Internal("remote rerank: no JSON object in LLM response".into())
    })?;
    let end = text.rfind('}').ok_or_else(|| {
        AppError::Internal("remote rerank: no JSON object in LLM response".into())
    })?;
    if end < start {
        return Err(AppError::Internal(
            "remote rerank: no JSON object in LLM response".into(),
        ));
    }
    Ok(&text[start..=end])
}

fn apply_llm_scores(
    results: &[(String, String, f32)],
    raw: &str,
    top_k: usize,
) -> Result<Vec<RerankedResult>> {
    let json = json_object_slice(raw)?;
    let parsed: LlmScoreResponse = serde_json::from_str(json)
        .map_err(|e| AppError::Internal(format!("remote rerank: failed to parse scores: {e}")))?;
    let scores: HashMap<String, f32> = parsed
        .scores
        .into_iter()
        .map(|row| (row.id, row.score))
        .collect();

    let mut reranked: Vec<RerankedResult> = results
        .iter()
        .enumerate()
        .map(|(idx, (id, content, retrieval_score))| {
            let rerank_score = scores.get(id).copied().unwrap_or(0.0);
            RerankedResult {
                id: id.clone(),
                content: content.clone(),
                retrieval_score: *retrieval_score,
                rerank_score,
                final_score: rerank_score,
                original_rank: idx + 1,
                new_rank: 0,
            }
        })
        .collect();

    reranked.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(Ordering::Equal)
    });
    for (idx, result) in reranked.iter_mut().enumerate() {
        result.new_rank = idx + 1;
    }
    reranked.truncate(top_k);
    Ok(reranked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::AppError;

    #[tokio::test]
    async fn rerank_with_llm_errors_when_llm_missing() {
        let ctx = Context::new_root();
        let err = rerank_with_llm(
            &ctx,
            "query",
            &[(String::from("a"), String::from("alpha"), 0.5)],
            1,
        )
        .await
        .expect_err("missing Llm must fail closed");
        match err {
            AppError::Configuration(msg) => {
                assert_eq!(msg, "Llm service is not provided for remote rerank");
            }
            other => panic!("expected Configuration, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rerank_with_llm_empty_results_skips_llm() {
        let ctx = Context::new_root();
        let out = rerank_with_llm(&ctx, "query", &[], 5)
            .await
            .expect("empty results must not require Llm");
        assert!(out.is_empty());
    }

    #[test]
    fn apply_llm_scores_sorts_joins_and_truncates() {
        let results = vec![
            (String::from("b"), String::from("beta"), 0.1),
            (String::from("a"), String::from("alpha"), 0.9),
        ];
        let ranked = apply_llm_scores(
            &results,
            r#"here is json:
```json
{"scores":[{"id":"a","score":0.2},{"id":"b","score":0.8}]}
```"#,
            1,
        )
        .expect("parse");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].id, "b");
        assert_eq!(ranked[0].content, "beta");
        assert_eq!(ranked[0].retrieval_score, 0.1);
        assert_eq!(ranked[0].rerank_score, 0.8);
        assert_eq!(ranked[0].final_score, 0.8);
        assert_eq!(ranked[0].original_rank, 1);
        assert_eq!(ranked[0].new_rank, 1);
    }

    #[tokio::test]
    async fn rerank_with_llm_complete_waterfall_short_circuit() {
        use ares_llm::{ConversationMessage, LLMClient, LLMResponse, Llm};
        use ares_types::types::ToolDefinition;
        use async_trait::async_trait;
        use cordis::EventsService;

        struct DummyCompleteClient;

        #[async_trait]
        impl LLMClient for DummyCompleteClient {
            async fn generate(&self, _prompt: &str) -> ares_types::types::Result<String> {
                Err(AppError::Internal("complete-only mock".into()))
            }
            async fn generate_with_system(
                &self,
                _system: &str,
                _prompt: &str,
            ) -> ares_types::types::Result<String> {
                Err(AppError::Internal("complete-only mock".into()))
            }
            async fn generate_with_history(
                &self,
                _messages: &[(String, String)],
            ) -> ares_types::types::Result<LLMResponse> {
                Err(AppError::Internal("complete-only mock".into()))
            }
            async fn generate_with_tools(
                &self,
                _prompt: &str,
                _tools: &[ToolDefinition],
            ) -> ares_types::types::Result<LLMResponse> {
                Err(AppError::Internal("complete-only mock".into()))
            }
            async fn generate_with_tools_and_history(
                &self,
                _messages: &[ConversationMessage],
                _tools: &[ToolDefinition],
            ) -> ares_types::types::Result<LLMResponse> {
                Err(AppError::Internal("complete-only mock".into()))
            }
            async fn stream(
                &self,
                _prompt: &str,
            ) -> ares_types::types::Result<
                Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>,
            > {
                Err(AppError::Internal("complete-only mock".into()))
            }
            async fn stream_with_system(
                &self,
                _system: &str,
                _prompt: &str,
            ) -> ares_types::types::Result<
                Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>,
            > {
                Err(AppError::Internal("complete-only mock".into()))
            }
            async fn stream_with_history(
                &self,
                _messages: &[(String, String)],
            ) -> ares_types::types::Result<
                Box<dyn futures::Stream<Item = ares_types::types::Result<String>> + Send + Unpin>,
            > {
                Err(AppError::Internal("complete-only mock".into()))
            }
            fn model_name(&self) -> &str {
                "complete-mock"
            }
        }

        let ctx = Context::new_root();
        ctx.provide(Llm::from_client(Arc::new(DummyCompleteClient)));
        let events = ctx.provide(EventsService::new());
        events.on_waterfall(
            cordis::events_catalog::ev::LLM_COMPLETE.to_string(),
            |_payload, _next| async move {
                Ok(serde_json::json!({
                    "content": "{\"scores\":[{\"id\":\"a\",\"score\":0.1},{\"id\":\"b\",\"score\":0.9}]}"
                }))
            },
        );
        let out = rerank_with_llm(
            &ctx,
            "query",
            &[
                (String::from("a"), String::from("alpha"), 0.4),
                (String::from("b"), String::from("beta"), 0.3),
            ],
            2,
        )
        .await
        .expect("short-circuit complete");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "b");
        assert_eq!(out[0].final_score, 0.9);
        assert_eq!(out[1].id, "a");
        assert_eq!(out[1].new_rank, 2);
    }
}
