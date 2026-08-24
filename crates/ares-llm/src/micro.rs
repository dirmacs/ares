//! Small-call orchestration over any [`LLMClient`](crate::client::LLMClient).
//!
//! A [`MicroTask`] is one tiny structured request: a fixed system template, a
//! minimal input payload, and a token budget. A [`MicroEngine`] runs such
//! tasks against a shared client, forces JSON-shaped answers, and treats
//! transport errors as correctness problems worth retrying.
//!
//! # Example
//!
//! ```ignore
//! use ares_llm::micro::{MicroEngine, MicroTask};
//!
//! let engine = MicroEngine::with_client(client);
//! let task = MicroTask {
//!     name: "extract-title",
//!     system: "You extract a JSON object with a single key \"title\".".into(),
//!     input: "Some long text".into(),
//!     max_tokens: 128,
//! };
//! let outcome = engine.run(&task).await?;
//! if let Some(value) = outcome.json {
//!     println!("title: {}", value["title"]);
//! }
//! ```

use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use serde_json::Value;

use crate::client::LLMClient;
use ares_types::types::Result;

/// One tiny structured request sent to a model.
///
/// The `system` template MUST stay fixed per task kind so its text is a
/// stable prefix that provider-side prompt caches can reuse across calls.
/// The `input` carries only the payload for THIS call; history is never
/// accumulated between calls.
pub struct MicroTask<'a> {
    /// Short label identifying the task kind; copied into the outcome.
    pub name: &'a str,
    /// Fixed instruction template for this task kind.
    pub system: String,
    /// Minimal payload for this single call.
    pub input: String,
    /// Advisory output token budget for this call.
    ///
    /// Informational only: [`LLMClient::generate_with_system`] takes no
    /// sampling parameters, so callers should pick clients configured with
    /// matching limits.
    pub max_tokens: u32,
}

/// Result of running one [`MicroTask`].
#[derive(Debug, Clone)]
pub struct MicroOutcome {
    /// Name of the task that produced this outcome.
    pub task: String,
    /// Parsed JSON value, present only when the answer text was valid JSON.
    pub json: Option<Value>,
    /// Raw answer text with optional code fences stripped.
    pub text: String,
    /// Wall-clock milliseconds spent on all attempts of this task.
    pub latency_ms: u128,
    /// Number of retries performed after failed attempts.
    pub retries: u32,
}

/// Bounded-concurrency runner for many tiny structured tasks over one client.
///
/// Retries are treated as part of correctness: a transport-level failure is
/// retried up to `max_retries` times before the error is surfaced.
pub struct MicroEngine {
    client: Arc<dyn LLMClient>,
    max_retries: u32,
    max_concurrency: usize,
}

impl MicroEngine {
    /// Create an engine with explicit retry and concurrency limits.
    pub fn new(client: Arc<dyn LLMClient>, max_retries: u32, max_concurrency: usize) -> Self {
        Self {
            client,
            max_retries,
            max_concurrency,
        }
    }

    /// Create an engine with defaults: 2 retries, 4 concurrent calls.
    pub fn with_client(client: Arc<dyn LLMClient>) -> Self {
        Self::new(client, 2, 4)
    }

    /// Run one task, retrying transport errors up to `max_retries` times.
    ///
    /// The answer text is trimmed of optional Markdown code fences before a
    /// JSON parse is attempted; [`MicroOutcome::json`] is `Some` only when
    /// that parse succeeds. The last error is returned when every attempt
    /// fails.
    pub async fn run(&self, task: &MicroTask<'_>) -> Result<MicroOutcome> {
        let started = Instant::now();
        let mut retries = 0u32;

        loop {
            match self
                .client
                .generate_with_system(&task.system, &task.input)
                .await
            {
                Ok(content) => {
                    let text = strip_code_fences(&content);
                    let json = serde_json::from_str::<Value>(text).ok();
                    return Ok(MicroOutcome {
                        task: task.name.to_string(),
                        json,
                        text: text.to_string(),
                        latency_ms: started.elapsed().as_millis(),
                        retries,
                    });
                }
                Err(err) => {
                    if retries >= self.max_retries {
                        return Err(err);
                    }
                    retries += 1;
                }
            }
        }
    }

    /// Run many tasks with bounded fan-out, preserving input order.
    ///
    /// At most `max_concurrency` calls are in flight at once. Results keep
    /// the order of `tasks` regardless of completion order; element `i` is
    /// the outcome of `tasks[i]`, or the last error after all retries.
    pub async fn run_all(&self, tasks: &[MicroTask<'_>]) -> Vec<Result<MicroOutcome>> {
        let finished = futures::stream::iter(tasks.iter().enumerate())
            .map(|(index, task)| async move { (index, self.run(task).await) })
            .buffer_unordered(self.max_concurrency.max(1))
            .collect::<Vec<(usize, Result<MicroOutcome>)>>()
            .await;

        let mut slots: Vec<Option<Result<MicroOutcome>>> = (0..tasks.len()).map(|_| None).collect();
        for (index, outcome) in finished {
            slots[index] = Some(outcome);
        }
        slots
            .into_iter()
            .map(|slot| slot.expect("slot filled"))
            .collect()
    }
}

/// Strip one optional Markdown code fence wrapper from model output.
fn strip_code_fences(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Skip an optional language tag such as `json` up to the first newline.
    let body = match rest.find('\n') {
        Some(newline) => &rest[newline + 1..],
        None => rest,
    };
    let body = body.trim_end();
    match body.strip_suffix("```") {
        Some(unwrapped) => unwrapped.trim_end(),
        None => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::AppError;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Step = std::result::Result<String, AppError>;

    /// Client whose `generate_with_system` follows a scripted behavior and
    /// counts calls; every other trait method fails as unused.
    struct BehaviorClient {
        behavior: Box<dyn Fn(usize) -> Step + Send + Sync>,
        calls: AtomicUsize,
    }

    impl BehaviorClient {
        fn new<F>(behavior: F) -> Self
        where
            F: Fn(usize) -> Step + Send + Sync + 'static,
        {
            Self {
                behavior: Box::new(behavior),
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LLMClient for BehaviorClient {
        async fn generate(&self, _prompt: &str) -> Result<String> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            (self.behavior)(call)
        }

        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<crate::client::LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ares_types::types::ToolDefinition],
        ) -> Result<crate::client::LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_tools_and_history(
            &self,
            _messages: &[crate::coordinator::ConversationMessage],
            _tools: &[ares_types::types::ToolDefinition],
        ) -> Result<crate::client::LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("unused".into()))
        }

        fn model_name(&self) -> &str {
            "micro-behavior-mock"
        }
    }

    fn task<'a>(name: &'a str, input: &str) -> MicroTask<'a> {
        MicroTask {
            name,
            system: "Return only a JSON object.".to_string(),
            input: input.to_string(),
            max_tokens: 64,
        }
    }

    #[tokio::test]
    async fn happy_path_parses_json() {
        let client = Arc::new(BehaviorClient::new(|_| Ok("{\"ok\": true}".to_string())));
        let engine = MicroEngine::with_client(client.clone());

        let outcome = engine.run(&task("check", "payload")).await.expect("run ok");

        assert_eq!(outcome.task, "check");
        assert_eq!(outcome.json, Some(serde_json::json!({ "ok": true })));
        assert_eq!(outcome.retries, 0);
        assert_eq!(outcome.text, "{\"ok\": true}");
        assert_eq!(client.call_count(), 1);
    }

    #[tokio::test]
    async fn fenced_json_is_unwrapped() {
        let client = Arc::new(BehaviorClient::new(|_| {
            Ok("```json\n{\"a\":1}\n```".to_string())
        }));
        let engine = MicroEngine::with_client(client);

        let outcome = engine
            .run(&task("fenced", "payload"))
            .await
            .expect("run ok");

        assert_eq!(outcome.json, Some(serde_json::json!({ "a": 1 })));
        assert_eq!(outcome.text, "{\"a\":1}");
    }

    #[tokio::test]
    async fn transport_error_retries_then_succeeds() {
        let client = Arc::new(BehaviorClient::new(|call| {
            if call < 2 {
                Err(AppError::External("transport down".into()))
            } else {
                Ok("{\"done\":true}".to_string())
            }
        }));
        let engine = MicroEngine::with_client(client.clone());

        let outcome = engine
            .run(&task("flaky", "payload"))
            .await
            .expect("third attempt succeeds");

        assert_eq!(outcome.retries, 2);
        assert_eq!(outcome.json, Some(serde_json::json!({ "done": true })));
        assert_eq!(client.call_count(), 3);
    }

    #[tokio::test]
    async fn exhausts_retries_returns_err() {
        let client = Arc::new(BehaviorClient::new(|_| {
            Err(AppError::External("still down".into()))
        }));
        let engine = MicroEngine::new(client.clone(), 2, 1);

        let result = engine.run(&task("dead", "payload")).await;

        assert!(result.is_err(), "expected Err after exhausting retries");
        assert_eq!(client.call_count(), 3, "initial attempt plus 2 retries");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn run_all_preserves_order_with_fanout() {
        const COUNT: usize = 6;
        let client = Arc::new(BehaviorClient::new(|call| {
            // Later-started calls finish first so completion order is the
            // reverse of submission order.
            std::thread::sleep(std::time::Duration::from_millis(
                ((COUNT - 1 - call) * 20) as u64,
            ));
            Ok(format!("{{\"call\":{}}}", call))
        }));
        let engine = MicroEngine::new(client, 0, COUNT);

        let names = ["t0", "t1", "t2", "t3", "t4", "t5"];
        let tasks: Vec<MicroTask<'_>> = names.iter().map(|name| task(name, "payload")).collect();

        let results = engine.run_all(&tasks).await;

        assert_eq!(results.len(), COUNT);
        for (index, result) in results.iter().enumerate() {
            let outcome = result.as_ref().unwrap_or_else(|err| {
                panic!("task {} should succeed: {}", index, err);
            });
            assert_eq!(outcome.task, names[index]);
        }
    }
}
