//! Small-call orchestration over any [`LLMClient`](crate::client::LLMClient).
//!
//! A [`MicroTask`] is one tiny structured request: a fixed system template, a
//! minimal input payload, and a token budget. A [`MicroEngine`] runs such
//! tasks against a shared client, forces JSON-shaped answers, and treats
//! transport errors as correctness problems worth retrying. An answer with
//! no directly parseable JSON is re-requested identically up to
//! `json_retries` times before substring salvage gets a chance.
//!
//! Identical deterministic-class requests (same model, template, and input)
//! are served from a bounded content-hash cache: see [`MicroCacheConfig`].
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

use std::collections::HashMap;
use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::Value;

use crate::client::LLMClient;
use ares_types::types::Result;

/// Default number of identical re-requests after an answer whose JSON could
/// not be parsed directly, before substring salvage runs.
pub(crate) const DEFAULT_JSON_RETRIES: u32 = 2;

/// Default maximum number of outcomes kept in the micro-call cache.
pub(crate) const DEFAULT_CACHE_ENTRIES: usize = 256;

/// Default time-to-live for a cached micro outcome.
pub(crate) const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// Tuning knobs for the [`MicroEngine`] response cache.
///
/// Identical deterministic-class requests — classify/tag style calls whose
/// answer came straight back as parseable JSON with zero retries — are served
/// from a bounded least-recently-used map keyed by a content hash over
/// `(model, system template, input)`. A hit skips the network entirely and
/// reports `latency_ms: 0` with [`MicroOutcome::cache_hit`] set. Answers
/// reached through retries or the salvage fallback are NEVER cached: the
/// repeated or repaired request proves the call was not deterministic-class.
///
/// Defaults suit read-mostly enrichment workloads; disable entirely for
/// intents where a fresh answer matters more than cost.
#[derive(Debug, Clone)]
pub struct MicroCacheConfig {
    /// Master switch; `false` makes every call go to the network.
    pub enabled: bool,
    /// Maximum number of cached outcomes; the least recently used entry is
    /// evicted when the bound is exceeded.
    pub max_entries: usize,
    /// How long a cached outcome stays fresh.
    pub ttl: Duration,
}

impl Default for MicroCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: DEFAULT_CACHE_ENTRIES,
            ttl: DEFAULT_CACHE_TTL,
        }
    }
}

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
    /// Number of retries performed after failed attempts: transport errors
    /// and answers whose JSON could not be parsed directly.
    pub retries: u32,
    /// `true` when this outcome was served from the response cache instead of
    /// a network call; cached outcomes always report `latency_ms: 0`.
    pub cache_hit: bool,
}

/// Bounded-concurrency runner for many tiny structured tasks over one client.
///
/// Retries are treated as part of correctness: a transport-level failure is
/// retried up to `max_retries` times before the error is surfaced, and an
/// answer with no directly parseable JSON is re-requested up to
/// `json_retries` times before the substring-salvage fallback runs.
pub struct MicroEngine {
    client: Arc<dyn LLMClient>,
    max_retries: u32,
    max_concurrency: usize,
    json_retries: u32,
    cache_config: MicroCacheConfig,
    cache: Mutex<LruOutcomeCache>,
}

/// Bounded least-recently-used store of micro outcomes keyed by content hash.
struct LruOutcomeCache {
    entries: HashMap<u64, CachedOutcome>,
    order: VecDeque<u64>,
}

/// One cached micro answer with the moment it was stored.
struct CachedOutcome {
    json: Option<Value>,
    text: String,
    stored_at: Instant,
}

impl LruOutcomeCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Fresh entry for `key`, promoted to most-recently-used; expired or
    /// missing entries are dropped and `None` is returned.
    fn get(&mut self, key: u64, ttl: Duration) -> Option<CachedOutcome> {
        let fresh = self
            .entries
            .get(&key)
            .is_some_and(|entry| entry.stored_at.elapsed() <= ttl);
        if !fresh {
            self.remove(key);
            return None;
        }
        self.promote(key);
        let entry = &self.entries[&key];
        Some(CachedOutcome {
            json: entry.json.clone(),
            text: entry.text.clone(),
            stored_at: entry.stored_at,
        })
    }

    fn insert(&mut self, key: u64, outcome: CachedOutcome, capacity: usize) {
        self.promote(key);
        self.entries.insert(key, outcome);
        while self.entries.len() > capacity.max(1) {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.entries.remove(&oldest);
                }
                None => break,
            }
        }
    }

    fn remove(&mut self, key: u64) {
        self.entries.remove(&key);
        if let Some(position) = self.order.iter().position(|k| *k == key) {
            self.order.remove(position);
        }
    }

    fn promote(&mut self, key: u64) {
        if let Some(position) = self.order.iter().position(|k| *k == key) {
            self.order.remove(position);
        }
        self.order.push_back(key);
    }
}

impl MicroEngine {
    /// Create an engine with explicit retry and concurrency limits.
    ///
    /// Malformed-JSON retries start at [`DEFAULT_JSON_RETRIES`]; override
    /// with [`MicroEngine::with_json_retries`].
    pub fn new(client: Arc<dyn LLMClient>, max_retries: u32, max_concurrency: usize) -> Self {
        Self {
            client,
            max_retries,
            max_concurrency,
            json_retries: DEFAULT_JSON_RETRIES,
            cache_config: MicroCacheConfig::default(),
            cache: Mutex::new(LruOutcomeCache::new()),
        }
    }

    /// Re-request an answer with no directly parseable JSON this many times,
    /// sending the identical request each time, before the substring-salvage
    /// fallback runs. Defaults to [`DEFAULT_JSON_RETRIES`].
    pub fn with_json_retries(mut self, json_retries: u32) -> Self {
        self.json_retries = json_retries;
        self
    }

    /// Override the response-cache knobs. Defaults cache up to
    /// [`DEFAULT_CACHE_ENTRIES`] outcomes for [`DEFAULT_CACHE_TTL`].
    pub fn with_cache_config(mut self, cache_config: MicroCacheConfig) -> Self {
        self.cache_config = cache_config;
        self
    }

    /// Create an engine with defaults: 2 transport retries, 2 malformed-JSON
    /// retries, 4 concurrent calls.
    pub fn with_client(client: Arc<dyn LLMClient>) -> Self {
        Self::new(client, 2, 4)
    }

    /// Run one task, retrying transport errors up to `max_retries` times.
    ///
    /// The answer text is trimmed of optional Markdown code fences before a
    /// strict JSON parse is attempted. An answer that does not parse is
    /// re-requested identically up to `json_retries` times; only then does
    /// the tolerant [`salvage_json`] fallback run on the last answer.
    /// [`MicroOutcome::json`] is `Some` only when the direct parse or the
    /// salvage succeeds. The last error is returned when every attempt fails.
    pub async fn run(&self, task: &MicroTask<'_>) -> Result<MicroOutcome> {
        let started = Instant::now();
        let mut retries = 0u32;
        let mut json_retries = 0u32;
        let key = self.cache_key(task);
        if self.cache_config.enabled {
            if let Some(hit) = self.cache.lock().get(key, self.cache_config.ttl) {
                tracing::debug!(task = task.name, "micro answer served from cache");
                return Ok(MicroOutcome {
                    task: task.name.to_string(),
                    json: hit.json,
                    text: hit.text,
                    latency_ms: 0,
                    retries: 0,
                    cache_hit: true,
                });
            }
        }

        loop {
            match self
                .client
                .generate_with_system(&task.system, &task.input)
                .await
            {
                Ok(content) => {
                    let text = strip_code_fences(&content);
                    match serde_json::from_str::<Value>(text) {
                        Ok(json) => {
                            if self.cache_config.enabled && retries == 0 && json_retries == 0 {
                                self.store(key, Some(json.clone()), text);
                            }
                            return Ok(MicroOutcome {
                                task: task.name.to_string(),
                                json: Some(json),
                                text: text.to_string(),
                                latency_ms: started.elapsed().as_millis(),
                                retries,
                                cache_hit: false,
                            });
                        }
                        Err(_) => {
                            // Silent degradation: a malformed answer costs one
                            // more identical request while budget remains;
                            // salvage is the final fallback and never errors.
                            if json_retries < self.json_retries {
                                json_retries += 1;
                                retries += 1;
                                tracing::debug!(
                                    task = task.name,
                                    json_retry = json_retries,
                                    "micro answer had no parseable JSON; re-requesting"
                                );
                                continue;
                            }
                            tracing::debug!(
                                task = task.name,
                                "micro answer still unparseable; falling back to salvage"
                            );
                            let json = salvage_json(text);
                            return Ok(MicroOutcome {
                                task: task.name.to_string(),
                                json,
                                text: text.to_string(),
                                latency_ms: started.elapsed().as_millis(),
                                retries,
                                cache_hit: false,
                            });
                        }
                    }
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

    /// Content-hash cache key over `(model, system template, input)`.
    fn cache_key(&self, task: &MicroTask<'_>) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.client.model_name().hash(&mut hasher);
        task.system.hash(&mut hasher);
        task.input.hash(&mut hasher);
        hasher.finish()
    }

    fn store(&self, key: u64, json: Option<Value>, text: &str) {
        self.cache.lock().insert(
            key,
            CachedOutcome {
                json,
                text: text.to_string(),
                stored_at: Instant::now(),
            },
            self.cache_config.max_entries,
        );
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

/// Three-stage JSON extraction from model output.
///
/// Models wrap or decorate JSON in prose more often than not, so parsing is
/// deliberately tolerant, tried in order of increasing intrusiveness:
///
/// 1. **Plain parse** — the text as-is.
/// 2. **Fence-strip parse** — after removing an optional Markdown code fence.
/// 3. **Substring salvage** — the span from the first `{` to the last `}`
///    (objects tried first), falling back to first `[` through last `]`
///    (arrays). The span is re-parsed, not assumed valid.
///
/// Returns `None` when no stage yields valid JSON; garbage never becomes an
/// error here — callers decide what a missing value means.
pub(crate) fn salvage_json(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    let stripped = strip_code_fences(text);
    if let Ok(value) = serde_json::from_str::<Value>(stripped) {
        return Some(value);
    }
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(start), Some(end)) = (stripped.find(open), stripped.rfind(close)) {
            if start < end {
                if let Ok(value) = serde_json::from_str::<Value>(&stripped[start..=end]) {
                    return Some(value);
                }
            }
        }
    }
    None
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

    #[tokio::test]
    async fn parse_failure_retries_then_succeeds() {
        let client = Arc::new(BehaviorClient::new(|call| {
            if call < 2 {
                Ok("no json in this answer".to_string())
            } else {
                Ok("{\"recovered\":true}".to_string())
            }
        }));
        let engine = MicroEngine::with_client(client.clone());

        let outcome = engine
            .run(&task("retry-json", "payload"))
            .await
            .expect("third attempt parses");

        assert_eq!(outcome.retries, 2, "two malformed-JSON re-requests");
        assert_eq!(
            outcome.json,
            Some(serde_json::json!({ "recovered": true })),
            "directly parsed answer must win over salvage"
        );
        assert_eq!(client.call_count(), 3);
    }

    #[tokio::test]
    async fn retries_exhausted_falls_back_to_salvage() {
        let client = Arc::new(BehaviorClient::new(|_| {
            Ok("Sure! {\"score\": 4} — hope that helps.".to_string())
        }));
        let engine = MicroEngine::with_client(client.clone());

        let outcome = engine
            .run(&task("salvage-after-retries", "payload"))
            .await
            .expect("salvage fallback yields an outcome");

        assert_eq!(
            outcome.retries, DEFAULT_JSON_RETRIES,
            "identical request repeated once per json retry before salvage"
        );
        assert_eq!(outcome.json, Some(serde_json::json!({ "score": 4 })));
        assert_eq!(
            client.call_count(),
            usize::try_from(DEFAULT_JSON_RETRIES).expect("retry count fits usize") + 1,
            "all json attempts happen before the single salvage pass"
        );
    }

    #[tokio::test]
    async fn zero_retries_goes_straight_to_salvage() {
        let client = Arc::new(BehaviorClient::new(|_| {
            Ok("Sure! {\"score\": 4} — hope that helps.".to_string())
        }));
        let engine = MicroEngine::with_client(client.clone()).with_json_retries(0);

        let outcome = engine
            .run(&task("salvage-now", "payload"))
            .await
            .expect("first unparseable answer salvages immediately");

        assert_eq!(outcome.retries, 0);
        assert_eq!(outcome.json, Some(serde_json::json!({ "score": 4 })));
        assert_eq!(client.call_count(), 1);
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

    #[test]
    fn salvage_parses_fenced_json() {
        let value = salvage_json("```json\n{\"a\": 1}\n```").expect("fenced json salvages");
        assert_eq!(value, serde_json::json!({ "a": 1 }));
    }

    #[test]
    fn salvage_extracts_json_from_prose() {
        let value =
            salvage_json("Here you go: {\"a\":1} hope that helps").expect("embedded json salvages");
        assert_eq!(value, serde_json::json!({ "a": 1 }));
    }

    #[test]
    fn salvage_prefers_object_over_array_delimiters() {
        let text = "[note] {\"kept\": true} [/note]";
        let value = salvage_json(text).expect("object wins over array");
        assert_eq!(
            value,
            serde_json::json!({ "kept": true }),
            "first {{ to last }} span is tried before bracket spans"
        );
    }

    #[test]
    fn salvage_parses_embedded_array() {
        let value = salvage_json("results: [1, 2, 3] done").expect("array salvages");
        assert_eq!(value, serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn salvage_returns_none_for_garbage() {
        assert_eq!(salvage_json("no json here at all"), None);
        assert_eq!(salvage_json("{not valid}"), None);
        assert_eq!(salvage_json(""), None);
    }

    #[tokio::test]
    async fn run_salvages_json_from_prose_answers() {
        let client = Arc::new(BehaviorClient::new(|_| {
            Ok("Sure! {\"score\": 4} — hope that helps.".to_string())
        }));
        let engine = MicroEngine::with_client(client);

        let outcome = engine
            .run(&task("salvage", "payload"))
            .await
            .expect("run ok");

        assert_eq!(outcome.json, Some(serde_json::json!({ "score": 4 })));
    }

    fn cache_config(max_entries: usize, ttl: Duration) -> MicroCacheConfig {
        MicroCacheConfig {
            enabled: true,
            max_entries,
            ttl,
        }
    }

    #[tokio::test]
    async fn identical_inputs_serve_cached_outcome() {
        let client = Arc::new(BehaviorClient::new(|call| {
            Ok(format!("{{\"call\":{}}}", call))
        }));
        let engine = MicroEngine::with_client(client.clone());

        let first = engine.run(&task("check", "payload")).await.expect("run ok");
        assert_eq!(client.call_count(), 1);
        assert!(!first.cache_hit);

        let second = engine.run(&task("check", "payload")).await.expect("run ok");
        assert!(second.cache_hit, "identical request must hit the cache");
        assert_eq!(second.latency_ms, 0, "cached outcome reports zero latency");
        assert_eq!(second.retries, 0);
        assert_eq!(
            second.json, first.json,
            "cache must serve the original answer"
        );
        assert_eq!(client.call_count(), 1, "cache hit must skip the network");

        let other = engine
            .run(&task("check", "different payload"))
            .await
            .expect("run ok");
        assert!(!other.cache_hit);
        assert_eq!(client.call_count(), 2, "a different input is a new key");
    }

    #[tokio::test]
    async fn ttl_expiry_refetches() {
        let client = Arc::new(BehaviorClient::new(|call| {
            Ok(format!("{{\"call\":{}}}", call))
        }));
        let engine = MicroEngine::with_client(client.clone())
            .with_cache_config(cache_config(8, Duration::from_millis(20)));

        engine.run(&task("ttl", "payload")).await.expect("run ok");
        let fresh = engine.run(&task("ttl", "payload")).await.expect("run ok");
        assert!(
            fresh.cache_hit,
            "within the TTL the answer comes from cache"
        );
        assert_eq!(client.call_count(), 1);

        tokio::time::sleep(Duration::from_millis(50)).await;
        let expired = engine.run(&task("ttl", "payload")).await.expect("run ok");
        assert!(!expired.cache_hit, "an expired entry must refetch");
        assert_eq!(client.call_count(), 2);
    }

    #[tokio::test]
    async fn lru_bounds_evict_oldest() {
        let client = Arc::new(BehaviorClient::new(|call| {
            Ok(format!("{{\"call\":{}}}", call))
        }));
        let engine = MicroEngine::with_client(client.clone())
            .with_cache_config(cache_config(2, Duration::from_secs(60)));

        engine.run(&task("lru", "a")).await.expect("run a");
        engine.run(&task("lru", "b")).await.expect("run b");
        // Refreshing `a` makes `b` the least recently used entry.
        let refreshed = engine.run(&task("lru", "a")).await.expect("run a again");
        assert!(refreshed.cache_hit);
        assert_eq!(client.call_count(), 2);

        engine.run(&task("lru", "c")).await.expect("run c");
        assert_eq!(client.call_count(), 3);
        assert_eq!(
            engine.cache.lock().entries.len(),
            2,
            "capacity bound holds after eviction"
        );

        // `b` was evicted; `a` survived because its recent hit renewed it.
        let survivor = engine.run(&task("lru", "a")).await.expect("run a third");
        assert!(survivor.cache_hit, "recently used entry must survive");
        assert_eq!(client.call_count(), 3);
        let evicted = engine.run(&task("lru", "b")).await.expect("run b again");
        assert!(!evicted.cache_hit, "oldest entry must have been evicted");
        assert_eq!(client.call_count(), 4);
    }

    #[tokio::test]
    async fn salvage_mutated_calls_not_cached() {
        let client = Arc::new(BehaviorClient::new(|_| {
            Ok("Sure! {\"score\": 4} — hope that helps.".to_string())
        }));
        let engine = MicroEngine::with_client(client.clone()).with_json_retries(0);

        let first = engine
            .run(&task("salvage", "payload"))
            .await
            .expect("salvage yields an outcome");
        assert_eq!(first.json, Some(serde_json::json!({ "score": 4 })));

        let second = engine
            .run(&task("salvage", "payload"))
            .await
            .expect("second identical call");
        assert_eq!(
            client.call_count(),
            2,
            "a salvaged answer proves the call was not deterministic-class"
        );
        assert!(!second.cache_hit);
    }

    #[tokio::test]
    async fn transport_retried_calls_not_cached() {
        let client = Arc::new(BehaviorClient::new(|call| {
            if call == 0 {
                Err(AppError::External("transport down".into()))
            } else {
                Ok("{\"late\":true}".to_string())
            }
        }));
        let engine = MicroEngine::with_client(client.clone());

        let first = engine.run(&task("flaky", "payload")).await.expect("run ok");
        assert_eq!(first.retries, 1);

        let second = engine.run(&task("flaky", "payload")).await.expect("run ok");
        assert_eq!(
            client.call_count(),
            3,
            "an answer reached through retries must not be cached"
        );
        assert!(!second.cache_hit);
    }
}
