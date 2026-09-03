//! Skill execution engine — turns a Skill definition into an agent run.

use ares_llm::{
    CapabilityRequirements, LLMResponse, Llm, MicroEngine, MicroOutcome, MicroTask,
    TenantModelPolicy,
};
use ares_store::run_history::{LogLlmCallRequest, LogToolCallRequest, RunHistoryStore};
use ares_store::skills::SkillStore;
use ares_store::tenant_allowlist::TenantAllowlistStore;
use ares_tools::Tools;
use ares_types::AppError;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::EmergencyStop;
use std::sync::Arc;

async fn resolve_model_tier(
    tenant_id: &str,
    tier_name: &str,
    pool: &PgPool,
) -> Option<(String, String)> {
    let store = ares_store::tenant_model_tiers::TenantModelTierStore::new(pool);
    match store.get(tenant_id, tier_name).await {
        Ok(Some(tier)) => Some((tier.provider_name, tier.model_name)),
        _ => None,
    }
}

fn estimated_cost_usd(prompt_tokens: i64, completion_tokens: i64) -> rust_decimal::Decimal {
    rust_decimal::Decimal::new((prompt_tokens + completion_tokens) * 2, 6)
}

const MAX_SKILL_CALL_DEPTH: usize = 8;
const RUN_HISTORY_STATUS_SUCCESS: &str = "success";
/// Stable marker prefixed to delegated-subtask cancellation aborts so
/// callers can classify them without parsing prose.
pub const SUBTASK_CANCELLED_MARKER: &str = "subtask_cancelled";

/// Sticky cancel token for one delegated subtask.
///
/// Once flipped the token stays cancelled — there is no un-cancel — so a
/// trigger racing a starting subtask still aborts it at its first call
/// boundary. Tokens are keyed `"{run_id}/{skill_id}"` in the engine's
/// registry; [`SkillEngine::cancel_subtask`] is the external trigger and
/// every execution loop checks its governing token before starting the
/// next LLM round or tool iteration.
#[derive(Debug, Default)]
pub struct SubtaskCancelToken {
    cancelled: AtomicBool,
}

impl SubtaskCancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Flip the token. Every later [`Self::check`] keeps failing.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Call-boundary gate: `Ok` while the subtask is active, one stable
    /// [`SUBTASK_CANCELLED_MARKER`] error once it has been cancelled.
    pub fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err(format!(
                "{SUBTASK_CANCELLED_MARKER}: delegated subtask was cancelled"
            ))
        } else {
            Ok(())
        }
    }
}

/// Fixed cache-friendly preamble for delegated-result self-critique rounds.
///
/// Kept byte-stable across calls and rounds so provider-side prompt caches
/// hit on the template; only the per-call tail (skill id, input, current
/// result) varies. Each round asks whether the answer addressed the task and
/// carries obvious errors or omissions; the model either outputs the
/// corrected final answer or returns the original verbatim.
const SELF_CHECK_TEMPLATE: &str = "You are double-checking the final answer of a delegated \
                                   sub-workflow step.\n\
                                   Ask yourself: (a) did the answer address the requested task? \
                                   (b) are there obvious errors or omissions?\n\
                                   If corrections are needed, output ONLY the corrected final \
                                   answer. Otherwise return the original answer verbatim.\n";

/// Opt-in ambient enrichment for assistant completions.
///
/// When enabled, every LLM step completion fires two parallel micro calls
/// (intent classification + keyword tags) over the micro-engine, and the
/// outcomes are attached as session metadata on the existing skill-step
/// record path (`run_llm_calls.response_payload["ambient_enrichment"]`).
/// Enrichment never delays or fails the completion itself: failures are
/// logged and silently skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AmbientEnrichmentConfig {
    /// Master switch. `false` (the default) issues no enrichment calls.
    pub enabled: bool,
}

/// Fixed system prompt for the intent-classification micro call.
const AMBIENT_INTENT_SYSTEM: &str = "You classify the user intent of an assistant answer. \
Reply with ONLY a JSON object of the form \
{\"intent\": \"question|command|summary|analysis|other\", \"confidence\": <number 0-1>}.";

/// Fixed system prompt for the keyword-tagging micro call.
const AMBIENT_TAGS_SYSTEM: &str = "You extract keyword tags from an assistant answer. \
Reply with ONLY a JSON object of the form {\"tags\": [\"tag\", ...]} with at most 5 tags.";

/// Run one ambient micro call. A transport failure after all retries yields
/// `None` — ambient enrichment is best-effort by contract.
async fn run_ambient_micro_call(
    engine: &MicroEngine,
    system: &str,
    input: String,
) -> Option<MicroOutcome> {
    let task = MicroTask {
        name: "ambient",
        system: system.to_string(),
        input,
        max_tokens: 128,
    };
    match engine.run(&task).await {
        Ok(outcome) => Some(outcome),
        Err(err) => {
            tracing::debug!(error = %err, "ambient enrichment call failed; skipping");
            None
        }
    }
}

/// Fold one micro outcome's parsed JSON fields into `target`.
fn merge_micro_outcome(outcome: &Option<MicroOutcome>, target: &mut serde_json::Value) {
    let Some(outcome) = outcome else { return };
    let Some(json) = &outcome.json else { return };
    let Some(object) = json.as_object() else {
        return;
    };
    let Some(target_object) = target.as_object_mut() else {
        return;
    };
    for (key, value) in object {
        target_object.insert(key.clone(), value.clone());
    }
}

/// Cap the completion text fed to the ambient micro calls.
fn truncate_ambient_input(text: &str) -> String {
    const AMBIENT_INPUT_MAX_CHARS: usize = 4_000;
    text.chars().take(AMBIENT_INPUT_MAX_CHARS).collect()
}

/// Fold ambient session metadata into a skill-step record's response
/// payload. Null metadata (disabled or fully failed enrichment) adds no key,
/// so the record stays byte-compatible with the pre-enrichment shape.
fn fold_ambient_into_response_payload(
    mut payload: serde_json::Value,
    ambient_metadata: serde_json::Value,
) -> serde_json::Value {
    if !ambient_metadata.is_null() {
        if let Some(object) = payload.as_object_mut() {
            object.insert("ambient_enrichment".to_string(), ambient_metadata);
        }
    }
    payload
}

fn default_step_input() -> serde_json::Value {
    serde_json::Value::Null
}

pub fn skill_result_token_counts(result: &serde_json::Value) -> (i64, i64) {
    fn walk(value: &serde_json::Value, input_tokens: &mut i64, output_tokens: &mut i64) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(usage) = object.get("usage").and_then(|usage| usage.as_object()) {
                    *input_tokens += usage
                        .get("prompt_tokens")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0);
                    *output_tokens += usage
                        .get("completion_tokens")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0);
                }
                for child in object.values() {
                    walk(child, input_tokens, output_tokens);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, input_tokens, output_tokens);
                }
            }
            _ => {}
        }
    }

    let mut input_tokens = 0;
    let mut output_tokens = 0;
    walk(result, &mut input_tokens, &mut output_tokens);
    (input_tokens, output_tokens)
}

fn validate_skill_call_depth(depth: usize) -> Result<(), String> {
    if depth > MAX_SKILL_CALL_DEPTH {
        return Err(format!(
            "Skill call depth exceeded maximum of {}",
            MAX_SKILL_CALL_DEPTH
        ));
    }
    Ok(())
}

/// One step inside a skill workflow.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum SkillStep {
    /// Call a tool by name with JSON arguments.
    ToolCall {
        #[serde(alias = "tool", alias = "name")]
        tool_name: String,
        #[serde(default = "default_step_input", alias = "arguments", alias = "input")]
        args: serde_json::Value,
    },
    /// Call an LLM with a prompt and a model tier.
    LlmCall {
        prompt: String,
        #[serde(alias = "model")]
        model_tier: String,
    },
    /// Execute another skill with optional JSON input.
    SkillCall {
        #[serde(alias = "skill", alias = "id")]
        skill_id: String,
        #[serde(default = "default_step_input", alias = "args", alias = "arguments")]
        input: serde_json::Value,
    },
    /// Conditional branch evaluated against execution context.
    Condition {
        expression: String,
        #[serde(default)]
        then_steps: Vec<SkillStep>,
    },
}

/// Engine that loads a [`Skill`] from the DB and executes its steps.
pub struct SkillEngine {
    pool: PgPool,
    tools: Arc<Tools>,
    llm: Arc<Llm>,
    ambient: AmbientEnrichmentConfig,
    self_check_rounds: Option<u32>,
    /// Sticky per-delegated-subtask cancel tokens keyed
    /// `"{run_id}/{skill_id}"`. Entries appear when an external trigger or
    /// registration names a subtask; execution only ever reads them.
    cancels: Mutex<HashMap<String, Arc<SubtaskCancelToken>>>,
}

impl SkillEngine {
    /// Create a new engine with Tools and Llm (no Overlay).
    ///
    /// Ambient enrichment defaults to off; enable it with
    /// [`SkillEngine::with_ambient_enrichment`].
    pub fn new(pool: PgPool, tools: Arc<Tools>, llm: Arc<Llm>) -> Self {
        Self {
            pool,
            tools,
            llm,
            ambient: AmbientEnrichmentConfig::default(),
            self_check_rounds: None,
            cancels: Mutex::new(HashMap::new()),
        }
    }

    /// Opt in to ambient enrichment of assistant completions (default off).
    ///
    /// When enabled, each LLM step fires parallel intent-classify and
    /// keyword-tag micro calls after the completion and attaches the parsed
    /// outcomes as session metadata on the existing skill-step record.
    pub fn with_ambient_enrichment(mut self, config: AmbientEnrichmentConfig) -> Self {
        self.ambient = config;
        self
    }

    /// Opt in to delegated-result self-critique rounds (default off).
    ///
    /// When set to `Some(n)` with `n >= 1`, each nested `SkillCall` result
    /// passes through up to `n` self-check rounds — one LLM call per round
    /// over a cache-stable template — before the result integrates. An LLM
    /// failure mid-loop keeps the last good answer silently.
    pub fn with_self_check_rounds(mut self, rounds: Option<u32>) -> Self {
        self.self_check_rounds = rounds.filter(|r| *r >= 1);
        self
    }
    /// Register (or fetch) the sticky cancel token for one delegated
    /// subtask. Idempotent: repeated registrations return the same token.
    pub fn register_subtask_cancel(&self, subtask_id: &str) -> Arc<SubtaskCancelToken> {
        self.cancels
            .lock()
            .expect("skill subtask cancel registry poisoned")
            .entry(subtask_id.to_string())
            .or_default()
            .clone()
    }

    /// External trigger path: flip the sticky cancel token for
    /// `subtask_id` (e.g. `"{run_id}/{skill_id}"`). Works before the
    /// subtask starts — a later registration observes the same flipped
    /// token — so a trigger racing delegation still aborts it at its first
    /// call boundary. Returns `true` on the first flip of that token,
    /// `false` when it was already cancelled.
    pub fn cancel_subtask(&self, subtask_id: &str) -> bool {
        let mut registry = self
            .cancels
            .lock()
            .expect("skill subtask cancel registry poisoned");
        let token = registry.entry(subtask_id.to_string()).or_default();
        !token.cancelled.swap(true, Ordering::SeqCst)
    }

    fn registered_cancel_token(&self, subtask_id: &str) -> Option<Arc<SubtaskCancelToken>> {
        self.cancels
            .lock()
            .expect("skill subtask cancel registry poisoned")
            .get(subtask_id)
            .cloned()
    }

    /// Delegate one nested `SkillCall`, gated by its per-subtask cancel
    /// token at every call boundary. A cancelled child aborts cleanly with
    /// the stable [`SUBTASK_CANCELLED_MARKER`] error; nothing it produced
    /// integrates into the parent context because the error unwinds before
    /// any context write. The pre-delegation gate catches triggers that
    /// landed before the child started; the child re-checks its own key at
    /// each of its own boundaries.
    async fn delegate_skill_call(
        &self,
        skill_id: &str,
        input: serde_json::Value,
        tenant_id: &str,
        run_id: &str,
        ctx: &Arc<cordis::Context>,
        depth: usize,
    ) -> Result<serde_json::Value, String> {
        ensure_execution_active(self, ctx, &format!("{run_id}/{skill_id}"))?;
        let check_input = self.self_check_rounds.map(|_| input.clone());
        let result = Box::pin(self.execute_skill_at_depth(
            skill_id,
            tenant_id,
            input,
            run_id,
            ctx,
            depth + 1,
        ))
        .await?;
        Ok(match check_input {
            Some(check_input) => {
                self.self_check_nested_result(ctx, skill_id, &check_input, result)
                    .await
            }
            None => result,
        })
    }

    fn scoped_tool_context(
        &self,
        ctx: &Arc<cordis::Context>,
        tenant_id: &str,
    ) -> Arc<cordis::Context> {
        ctx.isolate::<Tools>(tenant_id.to_string())
    }

    fn resolve_tenant_tool(
        &self,
        ctx: &Arc<cordis::Context>,
        tenant_id: &str,
        name: &str,
    ) -> Option<Arc<dyn ares_tools::Tool>> {
        let scoped = self.scoped_tool_context(ctx, tenant_id);
        let tools = scoped
            .get::<Tools>()
            .unwrap_or_else(|| Arc::clone(&self.tools));
        tools.resolve(&scoped, name)
    }

    async fn execute_tenant_tool(
        &self,
        ctx: &Arc<cordis::Context>,
        tenant_id: &str,
        name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let scoped = self.scoped_tool_context(ctx, tenant_id);
        let tools = scoped
            .get::<Tools>()
            .unwrap_or_else(|| Arc::clone(&self.tools));
        tools
            .execute(&scoped, name, args)
            .await
            .map_err(|e| match e {
                AppError::NotFound(_) => format!("Tool {name} not found"),
                e => format!("Tool {name} execution error: {e}"),
            })
    }

    /// Answer text a self-check round critiques: the `content` field of a
    /// standard step-context result, or a bare string result itself.
    /// Anything else carries no answer to check.
    fn self_check_answer(result: &serde_json::Value) -> Option<&str> {
        match result {
            serde_json::Value::String(text) => Some(text.as_str()),
            serde_json::Value::Object(_) => result.get("content").and_then(|c| c.as_str()),
            _ => None,
        }
    }

    /// Fold a corrected answer back into the result: structured results keep
    /// their shape (only `content` moves); bare strings are replaced whole.
    fn apply_self_check_answer(mut current: serde_json::Value, answer: &str) -> serde_json::Value {
        if current.is_object() && current.get("content").is_some() {
            current["content"] = serde_json::Value::String(answer.to_string());
            current
        } else {
            serde_json::Value::String(answer.to_string())
        }
    }

    /// Run up to `rounds` self-critique-and-fix rounds over a delegated skill
    /// result BEFORE any downstream gate sees it.
    ///
    /// Each round is exactly one LLM call whose prompt is the byte-stable
    /// [`SELF_CHECK_TEMPLATE`] followed by the delegated skill id, requested
    /// input, and current answer — identical request shape across rounds so
    /// provider-side prefix caches hit. A verbatim reply means the model found
    /// no corrections and ends the loop; an LLM error or empty answer keeps
    /// the last good answer silently and stops further rounds.
    async fn self_check_nested_result(
        &self,
        ctx: &Arc<cordis::Context>,
        delegated_skill_id: &str,
        delegated_input: &serde_json::Value,
        result: serde_json::Value,
    ) -> serde_json::Value {
        let Some(rounds) = self.self_check_rounds else {
            return result;
        };
        let Some(original_answer) = Self::self_check_answer(&result).map(str::to_string) else {
            return result;
        };
        let mut answer = original_answer.clone();
        for _ in 0..rounds {
            let prompt = format!(
                "{SELF_CHECK_TEMPLATE}Delegated sub-workflow: {delegated_skill_id}\n\
                 Requested input: {delegated_input}\n\
                 Current answer to check: {answer}",
            );
            match self.llm.complete(ctx, &prompt).await {
                Ok(next) if next.trim().is_empty() => {
                    tracing::debug!(
                        "self-check round returned an empty answer; keeping last good answer"
                    );
                    break;
                }
                // Verbatim reply: the model judged the answer sound.
                Ok(next) if next == answer => break,
                Ok(next) => answer = next,
                Err(err) => {
                    tracing::debug!(
                        error = %err,
                        "self-check LLM call failed; keeping last good answer"
                    );
                    break;
                }
            }
        }
        if answer == original_answer {
            return result;
        }
        Self::apply_self_check_answer(result, &answer)
    }

    /// Run the completion and, when ambient enrichment is enabled, fire the
    /// intent/tag micro calls in parallel afterwards.
    ///
    /// Returns the completion plus its session metadata (`Null` when
    /// disabled). Enrichment failures are logged and skipped — they never
    /// delay or fail the completion result.
    #[allow(clippy::too_many_arguments)]
    async fn complete_llm_step_with_metadata(
        &self,
        ctx: &Arc<cordis::Context>,
        messages: &[(String, String)],
        model_name: &str,
        run_id: Option<&str>,
        tenant_id: Option<&str>,
        step_index: Option<i32>,
        provider_name: Option<&str>,
    ) -> (Result<LLMResponse, String>, serde_json::Value) {
        // Preserve the existing skill behavior: concatenate message contents in order.
        let prompt = messages
            .iter()
            .map(|(_, content)| content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let request_ctx = if model_name.is_empty() || ctx.get::<ares_llm::ModelOverride>().is_some()
        {
            Arc::clone(ctx)
        } else {
            ctx.with_intercept(ares_llm::ModelOverride {
                model: model_name.to_string(),
            })
        };
        let content = self.llm.complete(&request_ctx, &prompt).await;
        let response = content
            .map_err(|e| format!("LLM generation failed: {e}"))
            .map(|content| LLMResponse {
                content,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: None,
                reasoning_content: None,
                response_id: None,
            });
        let metadata = match (&self.ambient, &response) {
            (AmbientEnrichmentConfig { enabled: true }, Ok(response)) => {
                self.run_ambient_enrichment(
                    ctx,
                    &response.content,
                    run_id,
                    tenant_id,
                    step_index,
                    provider_name.unwrap_or("default"),
                    model_name,
                )
                .await
            }
            _ => serde_json::Value::Null,
        };
        (response, metadata)
    }

    /// Fire the intent-classify and keyword-tag micro calls in parallel over
    /// the completion text and fold their parsed JSON into one session
    /// metadata object on the existing record path.
    ///
    /// Best-effort by contract: any micro-call failure is logged and that
    /// section is silently omitted.
    #[allow(clippy::too_many_arguments)]
    async fn run_ambient_enrichment(
        &self,
        ctx: &Arc<cordis::Context>,
        completion_text: &str,
        run_id: Option<&str>,
        tenant_id: Option<&str>,
        step_index: Option<i32>,
        provider_name: &str,
        model_name: &str,
    ) -> serde_json::Value {
        let client = match self
            .llm
            .get_client(ctx, CapabilityRequirements::default())
            .await
        {
            Ok(client) => client,
            Err(err) => {
                tracing::debug!(error = %err, "ambient enrichment client unavailable; skipping");
                return serde_json::Value::Null;
            }
        };
        let engine = MicroEngine::with_client(client);
        let input = truncate_ambient_input(completion_text);

        let intent = run_ambient_micro_call(&engine, AMBIENT_INTENT_SYSTEM, input.clone());
        let tags = run_ambient_micro_call(&engine, AMBIENT_TAGS_SYSTEM, input);
        let (intent, tags) = tokio::join!(intent, tags);

        let mut meta = serde_json::json!({
            "intent": {},
            "tags": {},
        });
        merge_micro_outcome(&intent, &mut meta["intent"]);
        merge_micro_outcome(&tags, &mut meta["tags"]);
        if let Some(run_id) = run_id {
            meta["run_id"] = serde_json::Value::String(run_id.to_string());
        }
        if let Some(tenant_id) = tenant_id {
            meta["tenant_id"] = serde_json::Value::String(tenant_id.to_string());
        }
        if let Some(step_index) = step_index {
            meta["step_index"] = serde_json::json!(step_index);
        }
        if !meta["intent"].as_object().is_some_and(|o| !o.is_empty())
            && !meta["tags"].as_object().is_some_and(|o| !o.is_empty())
        {
            return serde_json::Value::Null;
        }
        meta["provider"] = serde_json::Value::String(provider_name.to_string());
        meta["model"] = serde_json::Value::String(model_name.to_string());
        meta
    }

    /// Execute a skill by id for a tenant.
    ///
    /// Steps are run sequentially. Tool calls and LLM calls are executed
    /// for real and logged to `run_history` with their results.
    pub async fn execute_skill(
        &self,
        skill_id: &str,
        tenant_id: &str,
        input: serde_json::Value,
        run_id: &str,
        ctx: &Arc<cordis::Context>,
    ) -> Result<serde_json::Value, String> {
        self.execute_skill_at_depth(skill_id, tenant_id, input, run_id, ctx, 0)
            .await
    }

    async fn execute_skill_at_depth(
        &self,
        skill_id: &str,
        tenant_id: &str,
        input: serde_json::Value,
        run_id: &str,
        ctx: &Arc<cordis::Context>,
        depth: usize,
    ) -> Result<serde_json::Value, String> {
        validate_skill_call_depth(depth)?;
        let subtask_key = format!("{run_id}/{skill_id}");

        // 1. Load the skill definition
        let skill_store = SkillStore::new(&self.pool);
        let skill = skill_store
            .get_skill_for_tenant(skill_id, tenant_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Skill not found".to_string())?;

        // 2. Parse steps JSONB into SkillStep vec
        let steps: Vec<SkillStep> = serde_json::from_value(skill.steps)
            .map_err(|e| format!("Invalid skill steps: {}", e))?;

        // 3. Execute each step sequentially
        let mut context = serde_json::json!({"input": input});
        let mut step_index: i32 = 0;

        for step in steps {
            // Call boundary: never start the next LLM round or tool
            // iteration once cancellation latched. The governing token is
            // re-read from the registry at every boundary so an external
            // trigger racing the run is honored immediately.
            ensure_execution_active(self, ctx, &subtask_key)?;
            match step {
                SkillStep::ToolCall { tool_name, args } => {
                    tracing::info!("Step {}: tool_call {}", step_index, tool_name);
                    ensure_tenant_tool_allowed(&self.pool, tenant_id, &tool_name).await?;
                    let start = std::time::Instant::now();

                    // Execute via request-context Tools (tenant isolate → runtime → static).
                    let result = self
                        .execute_tenant_tool(ctx, tenant_id, &tool_name, args.clone())
                        .await?;

                    let latency_ms = start.elapsed().as_millis() as i64;

                    // Store result in context
                    context[&format!("step_{}", step_index)] =
                        successful_step_context(result.clone());

                    // Log the completed tool call
                    self.log_tool_call_result(
                        run_id,
                        tenant_id,
                        step_index,
                        &tool_name,
                        args,
                        Some(result),
                        latency_ms,
                    )
                    .await?;
                }
                SkillStep::LlmCall { prompt, model_tier } => {
                    tracing::info!("Step {}: llm_call (tier: {})", step_index, model_tier);
                    let start = std::time::Instant::now();

                    // Resolve model tier to concrete model
                    let (provider_name, model_name) =
                        resolve_model_tier(tenant_id, &model_tier, &self.pool)
                            .await
                            .unwrap_or_else(|| ("default".to_string(), model_tier.clone()));
                    ensure_tenant_model_allowed(&self.pool, tenant_id, &model_name).await?;

                    // Build messages and call Llm through the request context.
                    let messages = vec![("user".to_string(), prompt.clone())];
                    self.enforce_token_budget_before_llm_call(tenant_id).await?;
                    let (response, ambient_metadata) = self
                        .complete_llm_step_with_metadata(
                            ctx,
                            &messages,
                            &model_name,
                            Some(run_id),
                            Some(tenant_id),
                            Some(step_index),
                            Some(provider_name.as_str()),
                        )
                        .await;
                    let response = response?;
                    self.record_llm_token_budget_usage(tenant_id, run_id, &model_name, &response)
                        .await?;

                    let latency_ms = start.elapsed().as_millis() as i64;

                    // Store result
                    let result = serde_json::json!({
                        "content": response.content,
                        "usage": response.usage,
                    });
                    context[&format!("step_{}", step_index)] =
                        successful_step_context(result.clone());

                    // Log
                    self.log_llm_call_with_metadata(
                        run_id,
                        tenant_id,
                        step_index,
                        &provider_name,
                        &model_name,
                        response,
                        latency_ms,
                        ambient_metadata,
                    )
                    .await?;
                }
                SkillStep::SkillCall { skill_id, input } => {
                    tracing::info!("Step {}: skill_call {}", step_index, skill_id);
                    let result = Box::pin(
                        self.delegate_skill_call(&skill_id, input, tenant_id, run_id, ctx, depth),
                    )
                    .await?;
                    context[&format!("step_{}", step_index)] = successful_step_context(result);
                }
                SkillStep::Condition {
                    expression,
                    then_steps,
                } => {
                    tracing::info!("Step {}: condition {}", step_index, expression);
                    if let Some(ready_steps) = ready_then_steps(&expression, &then_steps, &context)
                    {
                        // Recursively execute then_steps
                        for (sub_idx, sub_step) in ready_steps.iter().enumerate() {
                            let sub_step_index = step_index + 1 + sub_idx as i32;
                            self.execute_sub_step(
                                sub_step,
                                ctx,
                                tenant_id,
                                run_id,
                                sub_step_index,
                                &mut context,
                                depth,
                                &subtask_key,
                            )
                            .await?;
                        }
                    }
                }
            }
            step_index += 1;
        }

        Ok(context)
    }

    /// Execute a single sub-step (used for conditional branches).
    async fn execute_sub_step(
        &self,
        step: &SkillStep,
        ctx: &Arc<cordis::Context>,
        tenant_id: &str,
        run_id: &str,
        step_index: i32,
        context: &mut serde_json::Value,
        depth: usize,
        cancel_key: &str,
    ) -> Result<(), String> {
        ensure_execution_active(self, ctx, cancel_key)?;
        match step {
            SkillStep::ToolCall { tool_name, args } => {
                tracing::info!("Sub-step {}: tool_call {}", step_index, tool_name);
                ensure_tenant_tool_allowed(&self.pool, tenant_id, tool_name).await?;
                let start = std::time::Instant::now();

                let result = self
                    .execute_tenant_tool(ctx, tenant_id, tool_name, args.clone())
                    .await?;

                let latency_ms = start.elapsed().as_millis() as i64;
                context[&format!("step_{}", step_index)] = successful_step_context(result.clone());

                self.log_tool_call_result(
                    run_id,
                    tenant_id,
                    step_index,
                    tool_name,
                    args.clone(),
                    Some(result),
                    latency_ms,
                )
                .await?;
            }
            SkillStep::LlmCall { prompt, model_tier } => {
                tracing::info!("Sub-step {}: llm_call (tier: {})", step_index, model_tier);
                let start = std::time::Instant::now();

                let (provider_name, model_name) =
                    resolve_model_tier(tenant_id, model_tier, &self.pool)
                        .await
                        .unwrap_or_else(|| ("default".to_string(), model_tier.clone()));
                ensure_tenant_model_allowed(&self.pool, tenant_id, &model_name).await?;

                let messages = vec![("user".to_string(), prompt.clone())];
                self.enforce_token_budget_before_llm_call(tenant_id).await?;
                let (response, ambient_metadata) = self
                    .complete_llm_step_with_metadata(
                        ctx,
                        &messages,
                        &model_name,
                        Some(run_id),
                        Some(tenant_id),
                        Some(step_index),
                        Some(provider_name.as_str()),
                    )
                    .await;
                let response = response?;
                self.record_llm_token_budget_usage(tenant_id, run_id, &model_name, &response)
                    .await?;

                let latency_ms = start.elapsed().as_millis() as i64;
                let result = serde_json::json!({
                    "content": response.content,
                    "usage": response.usage,
                });
                context[&format!("step_{}", step_index)] = successful_step_context(result.clone());

                self.log_llm_call_with_metadata(
                    run_id,
                    tenant_id,
                    step_index,
                    &provider_name,
                    &model_name,
                    response,
                    latency_ms,
                    ambient_metadata,
                )
                .await?;
            }
            SkillStep::SkillCall { skill_id, input } => {
                tracing::info!("Sub-step {}: skill_call {}", step_index, skill_id);
                let result = Box::pin(self.delegate_skill_call(
                    skill_id,
                    input.clone(),
                    tenant_id,
                    run_id,
                    ctx,
                    depth,
                ))
                .await?;
                context[&format!("step_{}", step_index)] = successful_step_context(result);
            }
            SkillStep::Condition {
                expression,
                then_steps,
            } => {
                tracing::info!("Sub-step {}: condition {}", step_index, expression);
                if let Some(ready_steps) = ready_then_steps(expression, then_steps, context) {
                    for (sub_idx, sub_step) in ready_steps.iter().enumerate() {
                        let sub_step_index = step_index + 1 + sub_idx as i32;
                        Box::pin(self.execute_sub_step(
                            sub_step,
                            ctx,
                            tenant_id,
                            run_id,
                            sub_step_index,
                            context,
                            depth,
                            cancel_key,
                        ))
                        .await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn enforce_token_budget_before_llm_call(&self, tenant_id: &str) -> Result<(), String> {
        let store = ares_store::token_budgets::TokenBudgetStore::new(&self.pool);
        let status = store
            .check_budget(tenant_id)
            .await
            .map_err(|e| e.to_string())?;
        if status.would_exceed {
            return Err(format!(
                "Tenant {} token budget exceeded ({} / {})",
                tenant_id, status.tokens_used, status.token_limit
            ));
        }
        Ok(())
    }

    async fn record_llm_token_budget_usage(
        &self,
        tenant_id: &str,
        run_id: &str,
        model: &str,
        response: &LLMResponse,
    ) -> Result<(), String> {
        let Some(usage) = response.usage.as_ref() else {
            return Ok(());
        };
        let store = ares_store::token_budgets::TokenBudgetStore::new(&self.pool);
        store
            .record_usage(
                tenant_id,
                Some(run_id),
                "skill",
                model,
                usage.prompt_tokens as i64,
                usage.completion_tokens as i64,
            )
            .await
            .map_err(|e| e.to_string())
    }

    async fn log_tool_call_result(
        &self,
        run_id: &str,
        tenant_id: &str,
        step_index: i32,
        tool_name: &str,
        args: serde_json::Value,
        result: Option<serde_json::Value>,
        latency_ms: i64,
    ) -> Result<(), String> {
        let store = RunHistoryStore::new(&self.pool);
        let req = LogToolCallRequest {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            tenant_id: tenant_id.to_string(),
            agent_name: "skill_executor".to_string(),
            step_index,
            tool_name: tool_name.to_string(),
            tool_type: "skill_step".to_string(),
            arguments: args,
            result,
            latency_ms,
            status: RUN_HISTORY_STATUS_SUCCESS.to_string(),
            error_message: None,
            created_at: chrono::Utc::now().timestamp(),
        };
        store
            .insert_tool_call(&req)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Log one LLM step on the existing `run_llm_calls` record path, with
    /// ambient-enrichment session metadata folded into `response_payload`
    /// when present.
    #[allow(clippy::too_many_arguments)]
    async fn log_llm_call_with_metadata(
        &self,
        run_id: &str,
        tenant_id: &str,
        step_index: i32,
        provider: &str,
        model: &str,
        response: LLMResponse,
        latency_ms: i64,
        ambient_metadata: serde_json::Value,
    ) -> Result<(), String> {
        let store = RunHistoryStore::new(&self.pool);
        let usage = response.usage.unwrap_or_default();
        let estimated_cost_usd =
            estimated_cost_usd(usage.prompt_tokens as i64, usage.completion_tokens as i64);
        let req = LogLlmCallRequest {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            tenant_id: tenant_id.to_string(),
            agent_name: "skill_executor".to_string(),
            step_index,
            provider: provider.to_string(),
            model: model.to_string(),
            prompt_tokens: usage.prompt_tokens as i64,
            completion_tokens: usage.completion_tokens as i64,
            total_tokens: usage.total_tokens as i64,
            estimated_cost_usd,
            latency_ms,
            cached_tokens: usage.cached_tokens,
            total_time_ms: Some(latency_ms),
            status: RUN_HISTORY_STATUS_SUCCESS.to_string(),
            error_message: None,
            request_payload: None,
            response_payload: Some(fold_ambient_into_response_payload(
                serde_json::json!({
                    "content": response.content,
                    "finish_reason": response.finish_reason,
                }),
                ambient_metadata,
            )),
            created_at: chrono::Utc::now().timestamp(),
        };
        store
            .insert_llm_call(&req)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn runtime_tool_error_allows_static_fallback(error: &AppError) -> bool {
    matches!(error, AppError::NotFound(_))
}

/// Call-boundary gate shared by every skill-execution loop: refuse to start
/// the next unit of work when the global emergency stop has latched or the
/// governing per-subtask cancel token flipped. Both abort with the stable
/// [`SUBTASK_CANCELLED_MARKER`] so callers classify them identically.
fn ensure_execution_active(
    engine: &SkillEngine,
    ctx: &Arc<cordis::Context>,
    subtask_key: &str,
) -> Result<(), String> {
    if let Some(stop) = ctx.get::<EmergencyStop>() {
        if stop.is_active() {
            return Err(format!(
                "{SUBTASK_CANCELLED_MARKER}: emergency stop is active"
            ));
        }
    }
    // Fresh registry read on every boundary: an external trigger racing the
    // run is honored at the very next LLM round or tool iteration.
    if let Some(token) = engine.registered_cancel_token(subtask_key) {
        token.check()?;
    }
    Ok(())
}

async fn ensure_tenant_tool_allowed(
    pool: &PgPool,
    tenant_id: &str,
    tool_name: &str,
) -> Result<(), String> {
    let store = TenantAllowlistStore::new(pool);
    match store.is_tool_allowed(tenant_id, tool_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "Tool '{}' is not allowed for tenant '{}'",
            tool_name, tenant_id
        )),
        Err(e) => Err(format!("Failed to check tool allowlist: {}", e)),
    }
}

async fn ensure_tenant_model_allowed(
    pool: &PgPool,
    tenant_id: &str,
    model_name: &str,
) -> Result<(), String> {
    let store = TenantAllowlistStore::new(pool);
    match store.is_model_allowed(tenant_id, model_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(TenantModelPolicy::denial_message(tenant_id, model_name)),
        Err(e) => Err(format!("Failed to check model allowlist: {}", e)),
    }
}

fn successful_step_context(result: serde_json::Value) -> serde_json::Value {
    match result {
        serde_json::Value::Object(mut fields) => {
            fields.entry("status").or_insert_with(|| {
                serde_json::Value::String(RUN_HISTORY_STATUS_SUCCESS.to_string())
            });
            serde_json::Value::Object(fields)
        }
        value => serde_json::json!({
            "status": RUN_HISTORY_STATUS_SUCCESS,
            "content": value,
        }),
    }
}

fn ready_then_steps<'a>(
    expression: &str,
    then_steps: &'a [SkillStep],
    context: &serde_json::Value,
) -> Option<&'a [SkillStep]> {
    evaluate_condition(expression, context).then_some(then_steps)
}

/// Evaluate a simple condition expression against the execution context.
///
/// Supported forms:
/// - `step_N.status == 'success'`
/// - `step_N.status != 'success'`
/// - `step_N.result == 'value'` (string comparison against step result["content"])
/// - `step_N.result != 'value'`
fn evaluate_condition(expression: &str, context: &serde_json::Value) -> bool {
    let expr = expression.trim();

    // Parse "left op right" where right is quoted
    let (left, op, right) = {
        let parts: Vec<&str> = expr.splitn(2, "==").collect();
        if parts.len() == 2 {
            (parts[0].trim(), "==", parts[1].trim())
        } else {
            let parts: Vec<&str> = expr.splitn(2, "!=").collect();
            if parts.len() == 2 {
                (parts[0].trim(), "!=", parts[1].trim())
            } else {
                return false;
            }
        }
    };

    let right_val = right.trim_matches('\'').trim_matches('"');

    // Parse left side like "step_0.status" or "step_0.result"
    let left_parts: Vec<&str> = left.split('.').collect();
    if left_parts.len() != 2 {
        return false;
    }
    let step_key = left_parts[0];
    let field = left_parts[1];

    let step_value = match context.get(step_key) {
        Some(v) => v,
        None => return false,
    };

    let actual = match field {
        "status" => step_value
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "result" => step_value
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => return false,
    };

    match op {
        "==" => actual == right_val,
        "!=" => actual != right_val,
        _ => false,
    }
}

impl cordis::Service for SkillEngine {
    fn name(&self) -> &'static str {
        "skill_engine"
    }
    fn init(&self, _ctx: &std::sync::Arc<cordis::Context>) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_llm::{ClientPool, LLMClient, ProviderRegistry};
    use ares_tools::Tool;
    use cordis::{Context, EventsService};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct EventProbeTool(Arc<AtomicBool>);

    #[async_trait::async_trait]
    impl Tool for EventProbeTool {
        fn name(&self) -> &str {
            "event-probe"
        }

        fn description(&self) -> &str {
            "tool used to prove tools.execute dispatch"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        async fn execute(&self, _args: serde_json::Value) -> ares_types::Result<serde_json::Value> {
            self.0.store(true, Ordering::SeqCst);
            Ok(json!({"reached": true}))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn skill_llm_call_uses_complete_waterfall() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "local",
            ares_llm::ProviderConfig::Ollama {
                api_key_env: "SKILL_ENGINE_TEST_KEY".to_string(),
                base_url: "http://127.0.0.1:9".to_string(),
                default_model: "test-model".to_string(),
            },
        );
        registry.register_model(
            "test-model",
            ares_llm::ModelConfig {
                provider: "local".to_string(),
                model: "test-model".to_string(),
                temperature: 0.0,
                max_tokens: 32,
            },
        );
        let llm = Arc::new(Llm::new(
            Arc::new(registry),
            Arc::new(ClientPool::with_defaults()),
            None,
        ));
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        events.on_waterfall("llm.complete".into(), |_payload, _next| async move {
            Ok(json!({"content": "cached"}))
        });
        let engine = SkillEngine::new(
            PgPool::connect_lazy("postgres://localhost/ares_test").expect("lazy pool"),
            Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new())),
            llm,
        );

        let (response, ambient) = engine
            .complete_llm_step_with_metadata(
                &ctx,
                &[(
                    "user".to_string(),
                    "ignored if provider is reached".to_string(),
                )],
                "test-model",
                None,
                None,
                None,
                None,
            )
            .await;
        let response = response.expect("llm.complete waterfall");
        assert!(ambient.is_null());
        assert_eq!(response.content, "cached");
        assert!(response.tool_calls.is_empty());
        assert_eq!(response.finish_reason, "stop");
        assert!(response.usage.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn skill_tool_call_uses_request_context_tools_execute_waterfall() {
        let reached = Arc::new(AtomicBool::new(false));
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        let _ = &reached;
        events.on_waterfall("tools.execute".into(), |_payload, _next| async move {
            Ok(json!({"result": {"cached": true}}))
        });

        let llm = Arc::new(Llm::new(
            Arc::new(ProviderRegistry::new()),
            Arc::new(ClientPool::with_defaults()),
            None,
        ));
        // Scope lookup misses (no per-tenant Tools on ctx) fall back to the
        // engine's own registry — which holds the event probe.
        let engine_tools =
            Tools::from_static([Arc::new(EventProbeTool(Arc::clone(&reached))) as Arc<dyn Tool>]);
        let engine = SkillEngine::new(
            PgPool::connect_lazy("postgres://localhost/ares_test").expect("lazy pool"),
            Arc::new(engine_tools),
            llm,
        );
        let result = engine
            .execute_tenant_tool(&ctx, "acme", "event-probe", json!({"x": 1}))
            .await
            .expect("tools.execute waterfall");

        assert_eq!(result, json!({"cached": true}));
        assert!(!reached.load(Ordering::SeqCst));
    }

    /// Shared live-test pool; an unreachable DB fails loudly with fix hints.
    async fn try_test_pool() -> PgPool {
        ares_test_support::pool().await
    }

    fn collect_ready_skill_calls<'a>(
        steps: &'a [SkillStep],
        context: &serde_json::Value,
        ready_skill_ids: &mut Vec<&'a str>,
    ) {
        for step in steps {
            match step {
                SkillStep::SkillCall { skill_id, .. } => ready_skill_ids.push(skill_id.as_str()),
                SkillStep::Condition {
                    expression,
                    then_steps,
                } => {
                    if let Some(ready_steps) = ready_then_steps(expression, then_steps, context) {
                        collect_ready_skill_calls(ready_steps, context, ready_skill_ids);
                    }
                }
                SkillStep::ToolCall { .. } | SkillStep::LlmCall { .. } => {}
            }
        }
    }

    #[test]
    fn runtime_tool_error_falls_back_only_when_tool_missing() {
        assert!(runtime_tool_error_allows_static_fallback(
            &AppError::NotFound("Runtime tool not found: calendar".to_string())
        ));
        assert!(!runtime_tool_error_allows_static_fallback(
            &AppError::External("runtime HTTP tool failed".to_string())
        ));
        assert!(!runtime_tool_error_allows_static_fallback(
            &AppError::Configuration("invalid runtime tool config".to_string())
        ));
        assert!(!runtime_tool_error_allows_static_fallback(
            &AppError::Unavailable("runtime tool disabled".to_string())
        ));
    }

    #[tokio::test]
    async fn ensure_tenant_tool_allowed_defaults_to_deny() {
        let pool = try_test_pool().await;
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let err = ensure_tenant_tool_allowed(&pool, &tenant_id, "calendar")
            .await
            .expect_err("missing allowlist row should deny");
        assert!(err.contains("calendar"));
    }

    #[tokio::test]
    async fn ensure_tenant_tool_allowed_accepts_enabled_row() {
        let pool = try_test_pool().await;
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let store = TenantAllowlistStore::new(&pool);
        store
            .allow_tool(&tenant_id, "calendar")
            .await
            .expect("allow tool");
        ensure_tenant_tool_allowed(&pool, &tenant_id, "calendar")
            .await
            .expect("enabled allowlist row should permit tool");
        let _ = store.deny_tool(&tenant_id, "calendar").await;
    }

    #[tokio::test]
    async fn ensure_tenant_model_allowed_defaults_to_deny() {
        let pool = try_test_pool().await;
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let err = ensure_tenant_model_allowed(&pool, &tenant_id, "gpt-4o")
            .await
            .expect_err("missing model allowlist row should deny");
        assert!(err.contains("gpt-4o"));
    }

    #[tokio::test]
    async fn ensure_tenant_model_allowed_accepts_enabled_row() {
        let pool = try_test_pool().await;
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let store = TenantAllowlistStore::new(&pool);
        store
            .allow_model(&tenant_id, "gpt-4o")
            .await
            .expect("allow model");
        ensure_tenant_model_allowed(&pool, &tenant_id, "gpt-4o")
            .await
            .expect("enabled allowlist row should permit model");
        let _ = store.deny_model(&tenant_id, "gpt-4o").await;
    }

    #[test]
    fn skill_llm_cost_estimate_uses_token_counts() {
        assert!(estimated_cost_usd(10, 5) > rust_decimal::Decimal::ZERO);
        assert_eq!(estimated_cost_usd(0, 0), rust_decimal::Decimal::ZERO);
    }

    #[test]
    fn skill_result_token_counts_sums_nested_llm_usage() {
        let result = serde_json::json!({
            "step_0": {
                "content": "top",
                "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14}
            },
            "step_1": {
                "nested": {
                    "usage": {"prompt_tokens": 3, "completion_tokens": 2, "total_tokens": 5}
                }
            }
        });

        assert_eq!(skill_result_token_counts(&result), (13, 6));
    }

    #[test]
    fn skill_run_history_status_matches_store_validation() {
        assert_eq!(RUN_HISTORY_STATUS_SUCCESS, "success");
    }

    #[test]
    fn skill_step_deserializes_internally_tagged_nested_chain() {
        let steps: Vec<SkillStep> = serde_json::from_value(json!([
            {
                "type": "condition",
                "expression": "step_0.status == 'success'",
                "then_steps": [
                    {
                        "type": "condition",
                        "expression": "step_1.result == 'ready'",
                        "then_steps": [
                            {
                                "type": "skill_call",
                                "skill": "child-skill",
                                "args": {"source": "nested-then"}
                            }
                        ]
                    }
                ]
            }
        ]))
        .expect("nested internally tagged skill steps should deserialize");

        let SkillStep::Condition { then_steps, .. } = &steps[0] else {
            panic!("top-level step should be a condition");
        };
        let SkillStep::Condition { then_steps, .. } = &then_steps[0] else {
            panic!("then_steps should contain the nested condition");
        };
        let SkillStep::SkillCall { skill_id, input } = &then_steps[0] else {
            panic!("nested condition should contain a skill call");
        };
        assert_eq!(skill_id, "child-skill");
        assert_eq!(input, &json!({"source": "nested-then"}));
    }

    #[test]
    fn successful_step_context_adds_status_without_losing_content() {
        let context = successful_step_context(json!({"content": "ready"}));
        assert_eq!(context["status"], RUN_HISTORY_STATUS_SUCCESS);
        assert_eq!(context["content"], "ready");
    }

    #[test]
    fn successful_step_context_preserves_existing_status() {
        let context = successful_step_context(json!({"status": "custom", "content": "ready"}));
        assert_eq!(context["status"], "custom");
        assert_eq!(context["content"], "ready");
    }

    #[test]
    fn status_conditions_match_successful_step_context() {
        let ready_steps = vec![SkillStep::SkillCall {
            skill_id: "child-skill".to_string(),
            input: json!({}),
        }];
        let context = json!({
            "step_0": successful_step_context(json!({"content": "ready"}))
        });
        assert!(ready_then_steps("step_0.status == 'success'", &ready_steps, &context).is_some());
    }

    #[test]
    fn nested_skill_call_is_ready_only_when_each_condition_matches() {
        let steps: Vec<SkillStep> = serde_json::from_value(json!([
            {
                "type": "condition",
                "expression": "step_0.status == 'success'",
                "then_steps": [
                    {
                        "type": "condition",
                        "expression": "step_1.result == 'ready'",
                        "then_steps": [
                            {"type": "skill_call", "skill_id": "child-skill"}
                        ]
                    }
                ]
            }
        ]))
        .expect("nested chain should deserialize");

        let mut ready = Vec::new();
        collect_ready_skill_calls(
            &steps,
            &json!({
                "step_0": {"status": "failed"},
                "step_1": {"content": "ready"}
            }),
            &mut ready,
        );
        assert!(
            ready.is_empty(),
            "nested skill_call metadata must not be considered executable when the outer condition fails"
        );

        collect_ready_skill_calls(
            &steps,
            &json!({
                "step_0": {"status": "success"},
                "step_1": {"content": "not-ready"}
            }),
            &mut ready,
        );
        assert!(
            ready.is_empty(),
            "outer condition readiness alone must not bypass the inner condition"
        );

        collect_ready_skill_calls(
            &steps,
            &json!({
                "step_0": {"status": "success"},
                "step_1": {"content": "ready"}
            }),
            &mut ready,
        );
        assert_eq!(ready, vec!["child-skill"]);
    }

    #[test]
    fn aliases_cover_existing_tool_and_input_shapes() {
        let steps: Vec<SkillStep> = serde_json::from_value(json!([
            {"type": "tool_call", "tool": "lookup", "input": {"q": "x"}},
            {"type": "llm_call", "prompt": "summarize", "model": "fast"},
            {"type": "skill_call", "id": "follow-up", "arguments": {"from": "alias"}}
        ]))
        .expect("aliased skill step fields should deserialize");

        let SkillStep::ToolCall { tool_name, args } = &steps[0] else {
            panic!("first step should be a tool call");
        };
        assert_eq!(tool_name, "lookup");
        assert_eq!(args, &json!({"q": "x"}));

        let SkillStep::LlmCall { model_tier, .. } = &steps[1] else {
            panic!("second step should be an LLM call");
        };
        assert_eq!(model_tier, "fast");

        let SkillStep::SkillCall { skill_id, input } = &steps[2] else {
            panic!("third step should be a skill call");
        };
        assert_eq!(skill_id, "follow-up");
        assert_eq!(input, &json!({"from": "alias"}));
    }

    #[test]
    fn skill_call_depth_guard_allows_boundary_and_rejects_next_level() {
        assert!(validate_skill_call_depth(MAX_SKILL_CALL_DEPTH).is_ok());
        let err = validate_skill_call_depth(MAX_SKILL_CALL_DEPTH + 1)
            .expect_err("depth above the maximum should be rejected");
        assert!(err.contains("Skill call depth exceeded maximum"));
    }

    /// Mock micro client: counts every `generate_with_system` call, answers
    /// with a scripted result (or error), and records the prompts it saw.
    struct AmbientMockClient {
        behavior: Mutex<AmbientBehavior>,
        calls: AtomicUsize,
    }

    enum AmbientBehavior {
        Respond(Box<dyn Fn(&str) -> String + Send + Sync>),
        Fail(String),
    }

    impl AmbientMockClient {
        fn respond<F>(make_answer: F) -> Self
        where
            F: Fn(&str) -> String + Send + Sync + 'static,
        {
            Self {
                behavior: Mutex::new(AmbientBehavior::Respond(Box::new(make_answer))),
                calls: AtomicUsize::new(0),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                behavior: Mutex::new(AmbientBehavior::Fail(message.to_string())),
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl LLMClient for AmbientMockClient {
        async fn generate(&self, prompt: &str) -> ares_types::Result<String> {
            // The completion waterfall calls this; answer with a
            // deterministic echo so tests never touch a real provider.
            Ok(format!("classification-of-{}", prompt))
        }

        async fn generate_with_system(
            &self,
            system: &str,
            prompt: &str,
        ) -> ares_types::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let Ok(guard) = self.behavior.lock() else {
                return Err(AppError::Internal("ambient mock poisoned".into()));
            };
            match &*guard {
                AmbientBehavior::Respond(make_answer) => {
                    // Echo the distinguishing prefix of the system prompt so
                    // tests can tell which micro task answered.
                    let which = if system.contains("classify") {
                        "intent"
                    } else {
                        "tags"
                    };
                    Ok(make_answer(which))
                }
                AmbientBehavior::Fail(message) => Err(AppError::Internal(message.clone())),
            }
        }

        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> ares_types::Result<LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ares_types::types::ToolDefinition],
        ) -> ares_types::Result<LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_tools_and_history(
            &self,
            _messages: &[ares_llm::ConversationMessage],
            _tools: &[ares_types::types::ToolDefinition],
        ) -> ares_types::Result<LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> ares_types::Result<
            Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>,
        > {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> ares_types::Result<
            Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>,
        > {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> ares_types::Result<
            Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>,
        > {
            Err(AppError::Internal("unused".into()))
        }

        fn model_name(&self) -> &str {
            "ambient-mock"
        }
    }

    /// Ambient mock pinned as the Llm test client — the SAME mock serves the
    /// completion (`generate` echoes the prompt) and the enrichment micro
    /// calls (`generate_with_system` answers per system-prompt kind).
    fn ambient_engine(
        mock: Arc<AmbientMockClient>,
        ambient: AmbientEnrichmentConfig,
    ) -> SkillEngine {
        SkillEngine::new(
            PgPool::connect_lazy("postgres://localhost/ares_test").expect("lazy pool"),
            Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new())),
            // Pin the completion AND enrichment paths to the same mock so no
            // real provider is ever reached.
            Arc::new(Llm::from_client(mock as Arc<dyn LLMClient>)),
        )
        .with_ambient_enrichment(ambient)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enrichment_off_no_calls() {
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        events.on_waterfall("llm.complete".into(), |payload, next| async move {
            next(payload).await
        });

        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let mock = Arc::new(AmbientMockClient::respond(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            "{}".to_string()
        }));
        let engine = ambient_engine(mock, AmbientEnrichmentConfig { enabled: false });

        let (response, metadata) = engine
            .complete_llm_step_with_metadata(
                &ctx,
                &[("user".to_string(), "hello there".to_string())],
                "",
                Some("run-1"),
                Some("acme"),
                Some(3),
                Some("local"),
            )
            .await;

        assert_eq!(
            response.expect("completion ok").content,
            "classification-of-hello there"
        );
        assert!(
            metadata.is_null(),
            "no session metadata should be attached when disabled"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "ambient micro client must not be called when disabled"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enrichment_on_attaches_metadata() {
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        events.on_waterfall("llm.complete".into(), |payload, next| async move {
            next(payload).await
        });

        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let mock = Arc::new(AmbientMockClient::respond(move |which| {
            seen.fetch_add(1, Ordering::SeqCst);
            if which == "intent" {
                r#"{"intent": "summary", "confidence": 0.87}"#.to_string()
            } else {
                r#"{"tags": ["alpha", "beta"]}"#.to_string()
            }
        }));
        let engine = ambient_engine(mock, AmbientEnrichmentConfig { enabled: true });

        let (response, metadata) = engine
            .complete_llm_step_with_metadata(
                &ctx,
                &[("user".to_string(), "hello there".to_string())],
                "",
                Some("run-1"),
                Some("acme"),
                Some(3),
                Some("local"),
            )
            .await;

        assert_eq!(
            response.expect("completion ok").content,
            "classification-of-hello there"
        );
        assert!(!metadata.is_null(), "session metadata should be attached");
        assert_eq!(metadata["intent"]["intent"], json!("summary"));
        assert_eq!(metadata["intent"]["confidence"], json!(0.87));
        assert_eq!(metadata["tags"]["tags"], json!(["alpha", "beta"]));
        assert_eq!(metadata["run_id"], json!("run-1"));
        assert_eq!(metadata["tenant_id"], json!("acme"));
        assert_eq!(metadata["step_index"], json!(3));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "both intent and tag micro calls must fire exactly once"
        );

        // The record path folds non-null session metadata under the
        // `response_payload` key "ambient_enrichment"; null stays absent.
        let folded = fold_ambient_into_response_payload(
            serde_json::json!({
                "content": "answer",
                "finish_reason": "stop",
            }),
            metadata.clone(),
        );
        let ambient = folded
            .get("ambient_enrichment")
            .expect("metadata folded into response_payload");
        assert_eq!(ambient["intent"]["intent"], json!("summary"));
        assert_eq!(ambient["tags"]["tags"], json!(["alpha", "beta"]));
        assert_eq!(folded["content"], json!("answer"));

        let unfolded = fold_ambient_into_response_payload(
            serde_json::json!({"content": "answer"}),
            serde_json::Value::Null,
        );
        assert!(
            unfolded.get("ambient_enrichment").is_none(),
            "null metadata must not add an ambient key"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enrichment_failure_silent() {
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());
        events.on_waterfall("llm.complete".into(), |payload, next| async move {
            next(payload).await
        });

        let mock = Arc::new(AmbientMockClient::failing("provider down"));
        let engine = ambient_engine(mock, AmbientEnrichmentConfig { enabled: true });

        let (response, metadata) = engine
            .complete_llm_step_with_metadata(
                &ctx,
                &[("user".to_string(), "hello there".to_string())],
                "",
                Some("run-2"),
                Some("acme"),
                Some(1),
                Some("local"),
            )
            .await;

        assert_eq!(
            response
                .expect("completion must succeed despite enrichment failure")
                .content,
            "classification-of-hello there"
        );
        assert!(
            metadata.is_null(),
            "failed enrichment is silently skipped, not surfaced"
        );

        // The record still lands on the existing path; folding the null
        // metadata leaves no ambient_enrichment key behind.
        let folded = fold_ambient_into_response_payload(
            serde_json::json!({
                "content": "classification-of-hello there",
                "finish_reason": "stop",
            }),
            metadata,
        );
        assert!(
            folded.get("ambient_enrichment").is_none(),
            "no ambient key when enrichment failed"
        );
        assert_eq!(folded["content"], json!("classification-of-hello there"));
    }

    /// Scripted mock for self-check rounds: `generate` pops the next scripted
    /// outcome per call and records every prompt it saw.
    struct SelfCheckMockClient {
        replies: Mutex<std::collections::VecDeque<Result<String, String>>>,
        prompts: Mutex<Vec<String>>,
    }

    impl SelfCheckMockClient {
        fn scripted(replies: Vec<Result<String, String>>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.into()),
                prompts: Mutex::new(Vec::new()),
            })
        }

        fn recorded_prompts(&self) -> Vec<String> {
            self.prompts.lock().expect("prompts").clone()
        }

        fn record(&self, prompt: &str) {
            self.prompts
                .lock()
                .expect("prompts")
                .push(prompt.to_string());
        }

        fn pop_reply(&self) -> Result<String, AppError> {
            let reply = self
                .replies
                .lock()
                .expect("replies")
                .pop_front()
                .unwrap_or_else(|| Ok(String::new()));
            reply.map_err(AppError::Internal)
        }
    }

    #[async_trait::async_trait]
    impl LLMClient for SelfCheckMockClient {
        async fn generate(&self, prompt: &str) -> ares_types::Result<String> {
            self.record(prompt);
            self.pop_reply()
        }

        async fn generate_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> ares_types::Result<String> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> ares_types::Result<LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ares_types::types::ToolDefinition],
        ) -> ares_types::Result<LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_tools_and_history(
            &self,
            _messages: &[ares_llm::ConversationMessage],
            _tools: &[ares_types::types::ToolDefinition],
        ) -> ares_types::Result<LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> ares_types::Result<
            Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>,
        > {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> ares_types::Result<
            Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>,
        > {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> ares_types::Result<
            Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>,
        > {
            Err(AppError::Internal("unused".into()))
        }

        fn model_name(&self) -> &str {
            "self-check-mock"
        }
    }

    /// Engine pinned to the scripted self-check mock; the lazy pool is never
    /// touched because only `self_check_nested_result` runs.
    fn self_check_engine(mock: Arc<SelfCheckMockClient>, rounds: Option<u32>) -> SkillEngine {
        SkillEngine::new(
            PgPool::connect_lazy("postgres://localhost/ares_test").expect("lazy pool"),
            Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new())),
            Arc::new(Llm::from_client(mock as Arc<dyn LLMClient>)),
        )
        .with_self_check_rounds(rounds)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn self_check_off_passthrough_identical() {
        // No LLM service traffic at all: the mock is never consulted.
        let ctx = Context::new_root();

        let result = json!({
            "status": "success",
            "content": "flawed answer",
            "usage": {"prompt_tokens": 1},
        });
        let engine = self_check_engine(SelfCheckMockClient::scripted(vec![]), None);
        let out = engine
            .self_check_nested_result(
                &ctx,
                "child-skill",
                &json!({"q": "capital of France"}),
                result.clone(),
            )
            .await;
        assert_eq!(
            out, result,
            "off must return the delegated result unchanged"
        );

        // Explicit zero behaves identically to off.
        let engine_zero = self_check_engine(SelfCheckMockClient::scripted(vec![]), Some(0));
        let out_zero = engine_zero
            .self_check_nested_result(&ctx, "child-skill", &json!({"q": "x"}), result.clone())
            .await;
        assert_eq!(out_zero, result);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_round_fixes_flawed_answer() {
        let ctx = Context::new_root();
        let mock =
            SelfCheckMockClient::scripted(vec![Ok("The capital of France is Paris.".into())]);
        let engine = self_check_engine(Arc::clone(&mock), Some(1));

        let flawed = json!({"status": "success", "content": "The capital of France is Lyon."});
        let out = engine
            .self_check_nested_result(
                &ctx,
                "geo-skill",
                &json!({"task": "capital of France"}),
                flawed,
            )
            .await;

        assert_eq!(out["content"], json!("The capital of France is Paris."));
        assert_eq!(
            out["status"],
            json!("success"),
            "non-content fields stay intact"
        );

        let prompts = mock.recorded_prompts();
        assert_eq!(
            prompts.len(),
            1,
            "exactly one round means exactly one LLM call"
        );
        assert!(
            prompts[0].starts_with(SELF_CHECK_TEMPLATE),
            "round prompt must lead with the cache-stable template"
        );
        assert!(prompts[0].contains("geo-skill"));
        assert!(prompts[0].contains("The capital of France is Lyon."));

        // Bare string results are replaced whole.
        let bare = engine
            .self_check_nested_result(&ctx, "s", &json!({}), json!("stale text"))
            .await;
        assert_eq!(
            bare,
            json!("stale text"),
            "verbatim reply keeps the original"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn llm_failure_keeps_last_good_answer() {
        let ctx = Context::new_root();
        // Round 1 fixes the flaw; round 2's call errors out.
        let mock = SelfCheckMockClient::scripted(vec![
            Ok("corrected answer".into()),
            Err("provider down".into()),
        ]);
        let engine = self_check_engine(Arc::clone(&mock), Some(2));

        let flawed = json!({"status": "success", "content": "broken draft"});
        let out = engine
            .self_check_nested_result(&ctx, "child", &json!({}), flawed.clone())
            .await;

        assert_eq!(
            out["content"],
            json!("corrected answer"),
            "failure mid-loop must keep the last good answer"
        );
        assert_eq!(mock.recorded_prompts().len(), 2, "both rounds attempted");

        // A failure on the FIRST round degrades to the untouched input.
        let first_fails = SelfCheckMockClient::scripted(vec![Err("down".into())]);
        let degraded = self_check_engine(first_fails.clone(), Some(3))
            .self_check_nested_result(&ctx, "child", &json!({}), flawed.clone())
            .await;
        assert_eq!(
            degraded, flawed,
            "first-round failure passes the original through"
        );
        assert_eq!(first_fails.recorded_prompts().len(), 1);
    }
    // -------------------------------------------------------------------
    // Per-subtask cancel tokens
    // -------------------------------------------------------------------

    /// Fixed run id for cancellation tests; subtask keys are
    /// `"{CANCEL_TEST_RUN_ID}/{skill_id}"`.
    const CANCEL_TEST_RUN_ID: &str = "run-cancel-test";

    /// Shared fixture for the cancellation tests: a real test-pool engine
    /// whose `llm.complete` waterfall records every prompt and can flip
    /// cancel tokens at exact round boundaries.
    struct CancelFixture {
        engine: Arc<SkillEngine>,
        ctx: Arc<Context>,
        pool: PgPool,
        tenant_id: String,
        seen_prompts: Arc<Mutex<Vec<String>>>,
    }

    impl CancelFixture {
        async fn build() -> Self {
            let pool = try_test_pool().await;
            let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
            TenantAllowlistStore::new(&pool)
                .allow_model(&tenant_id, "test-model")
                .await
                .expect("allow model for tenant");

            // get_client resolves before the waterfall short-circuits, so a
            // fully-registered unreachable provider keeps the hook in charge.
            let mut registry = ProviderRegistry::new();
            registry.register_provider(
                "local",
                ares_llm::ProviderConfig::Ollama {
                    api_key_env: "SKILL_ENGINE_CANCEL_TEST_KEY".to_string(),
                    base_url: "http://127.0.0.1:9".to_string(),
                    default_model: "test-model".to_string(),
                },
            );
            registry.register_model(
                "test-model",
                ares_llm::ModelConfig {
                    provider: "local".to_string(),
                    model: "test-model".to_string(),
                    temperature: 0.0,
                    max_tokens: 32,
                },
            );
            let llm = Arc::new(Llm::new(
                Arc::new(registry),
                Arc::new(ClientPool::with_defaults()),
                None,
            ));
            let engine = Arc::new(SkillEngine::new(
                pool.clone(),
                Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new())),
                llm,
            ));
            let ctx = Context::new_root();
            ctx.provide(EventsService::new());

            Self {
                engine,
                ctx,
                pool,
                tenant_id,
                seen_prompts: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// Install the recording waterfall. `on_prompt` runs after each LLM
        /// round is recorded — flip a token there to cancel between rounds.
        fn install_prompt_hook(
            &self,
            on_prompt: impl Fn(&str, &SkillEngine) + Send + Sync + Clone + 'static,
        ) {
            let events = self.ctx.get::<EventsService>().expect("events service");
            let prompts_for_handler = Arc::clone(&self.seen_prompts);
            let engine_for_handler = Arc::clone(&self.engine);
            events.on_waterfall("llm.complete".into(), move |payload, _next| {
                let prompts = Arc::clone(&prompts_for_handler);
                let engine = Arc::clone(&engine_for_handler);
                let on_prompt = on_prompt.clone();
                async move {
                    let prompt = payload
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    prompts.lock().expect("prompt log").push(prompt.clone());
                    on_prompt(&prompt, &engine);
                    Ok(json!({"content": format!("answer-for-{prompt}")}))
                }
            });
        }

        /// Insert a skill owned by the fixture tenant; returns its id.
        async fn create_skill(&self, label: &str, steps: serde_json::Value) -> String {
            let label = format!("{label}-{}", uuid::Uuid::new_v4());
            let created = SkillStore::new(&self.pool)
                .create_skill(&ares_store::skills::CreateSkillRequest {
                    tenant_id: self.tenant_id.clone(),
                    name: label.clone(),
                    display_name: label.clone(),
                    description: None,
                    skill_type: "workflow".to_string(),
                    steps,
                    input_schema: None,
                    output_schema: None,
                    tools: None,
                    is_public: false,
                    created_by: None,
                })
                .await
                .expect("create skill");
            created.id
        }

        async fn delete_skill(&self, skill_id: &str) {
            let _ = SkillStore::new(&self.pool)
                .delete_skill_for_tenant(&self.tenant_id, skill_id)
                .await;
        }
        /// Insert the `agent_runs` row `run_llm_calls.run_id` requires.
        async fn seed_run_row(&self, run_id: &str) {
            sqlx::query(
                "INSERT INTO tenants (id, name, tier, created_at, updated_at) \
                 VALUES ($1, $2, 'free', 1, 1) ON CONFLICT (id) DO NOTHING",
            )
            .bind(&self.tenant_id)
            .bind(&self.tenant_id)
            .execute(&self.pool)
            .await
            .expect("seed tenant row");
            sqlx::query(
                "INSERT INTO agent_runs (id, tenant_id, agent_name, status, \
                     input_tokens, output_tokens, duration_ms, created_at) \
                 VALUES ($1, $2, 'skill_cancel_test', 'completed', 0, 0, 0, 1) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(run_id)
            .bind(&self.tenant_id)
            .execute(&self.pool)
            .await
            .expect("seed agent run row");
        }

        fn prompts(&self) -> Vec<String> {
            self.seen_prompts.lock().expect("prompt log").clone()
        }
    }

    fn two_llm_rounds(first: &str, second: &str) -> serde_json::Value {
        json!([
            {"type": "llm_call", "prompt": first, "model_tier": "test-model"},
            {"type": "llm_call", "prompt": second, "model_tier": "test-model"},
        ])
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_token_aborts_subtask_between_rounds() {
        let fixture = CancelFixture::build().await;
        let skill_id = fixture
            .create_skill(
                "cancel-between-rounds",
                two_llm_rounds("cancel-round-0", "cancel-round-1"),
            )
            .await;
        let subtask_id = format!("{}/{}", CANCEL_TEST_RUN_ID, skill_id);
        let hook_subtask_id = subtask_id.clone();
        fixture.install_prompt_hook(move |prompt, engine| {
            if prompt.contains("round-0") {
                // External trigger racing the run: flips between rounds.
                engine.cancel_subtask(&hook_subtask_id);
            }
        });

        fixture.seed_run_row(CANCEL_TEST_RUN_ID).await;
        let err = fixture
            .engine
            .execute_skill(
                &skill_id,
                &fixture.tenant_id,
                json!({}),
                CANCEL_TEST_RUN_ID,
                &fixture.ctx,
            )
            .await
            .expect_err("cancelled execution must abort");

        assert!(
            err.contains(SUBTASK_CANCELLED_MARKER),
            "abort must carry the stable marker, got {err}"
        );
        assert_eq!(
            fixture.prompts(),
            vec!["cancel-round-0".to_string()],
            "execution must abort between rounds, not mid-call or after"
        );
        assert!(
            !fixture.engine.cancel_subtask(&subtask_id),
            "sticky token flips exactly once"
        );

        fixture.delete_skill(&skill_id).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_result_not_integrated() {
        let fixture = CancelFixture::build().await;
        let child_id = fixture
            .create_skill(
                "cancel-child",
                two_llm_rounds("child-round-0", "child-round-1"),
            )
            .await;
        let parent_steps = json!([
            {"type": "llm_call", "prompt": "parent-zero", "model_tier": "test-model"},
            {"type": "skill_call", "skill_id": child_id, "input": {}},
            {"type": "llm_call", "prompt": "parent-tail", "model_tier": "test-model"},
        ]);
        let parent_id = fixture.create_skill("cancel-parent", parent_steps).await;
        let hook_child_key = format!("{}/{}", CANCEL_TEST_RUN_ID, child_id);
        fixture.install_prompt_hook(move |prompt, engine| {
            if prompt.contains("child-round-0") {
                // The child produced one partial round; abort it before its
                // result could integrate into the parent context.
                engine.cancel_subtask(&hook_child_key);
            }
        });

        fixture.seed_run_row(CANCEL_TEST_RUN_ID).await;
        let err = fixture
            .engine
            .execute_skill(
                &parent_id,
                &fixture.tenant_id,
                json!({}),
                CANCEL_TEST_RUN_ID,
                &fixture.ctx,
            )
            .await
            .expect_err("cancelled delegation must unwind the parent run");

        assert!(
            err.contains(SUBTASK_CANCELLED_MARKER),
            "unwind must carry the stable marker, got {err}"
        );
        assert_eq!(
            fixture.prompts(),
            vec!["parent-zero".to_string(), "child-round-0".to_string(),],
            "parent tail and child round 1 must never start after cancellation"
        );
        assert!(
            !err.contains("answer-for-child-round-0"),
            "partial child output must not leak into the reported result"
        );

        fixture.delete_skill(&parent_id).await;
        fixture.delete_skill(&child_id).await;
    }
}
