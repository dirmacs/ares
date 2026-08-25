//! Skill execution engine — turns a Skill definition into an agent run.

use ares_store::run_history::{LogLlmCallRequest, LogToolCallRequest, RunHistoryStore};
use ares_store::skills::SkillStore;
use ares_store::tenant_allowlist::TenantAllowlistStore;
use ares_llm::{
    CapabilityRequirements, Llm, LLMResponse, MicroEngine, MicroOutcome, MicroTask,
    TenantModelPolicy,
};
use ares_tools::Tools;
use ares_types::AppError;
use sqlx::PgPool;
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

/// Opt-in ambient enrichment for assistant completions.
///
/// When enabled, every LLM step completion fires two parallel micro calls
/// (intent classification + keyword tags) over the micro-engine, and the
/// outcomes are attached as session metadata on the existing skill-step
/// record path (`run_llm_calls.response_payload["ambient_enrichment"]`).
/// Enrichment never delays or fails the completion itself: failures are
/// logged and silently skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
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
    let Some(object) = json.as_object() else { return };
    let Some(target_object) = target.as_object_mut() else { return };
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
}

impl SkillEngine {
    /// Create a new engine with Tools and Llm (no Overlay).
    ///
    /// Ambient enrichment defaults to off; enable it with
    /// [`SkillEngine::with_ambient_enrichment`].
    pub fn new(
        pool: PgPool,
        tools: Arc<Tools>,
        llm: Arc<Llm>,
    ) -> Self {
        Self {
            pool,
            tools,
            llm,
            ambient: AmbientEnrichmentConfig::default(),
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
        let tools = scoped.get::<Tools>().unwrap_or_else(|| Arc::clone(&self.tools));
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
        let tools = scoped.get::<Tools>().unwrap_or_else(|| Arc::clone(&self.tools));
        tools
            .execute(&scoped, name, args)
            .await
            .map_err(|e| match e {
                AppError::NotFound(_) => format!("Tool {name} not found"),
                e => format!("Tool {name} execution error: {e}"),
            })
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
        let request_ctx = if model_name.is_empty() || ctx.get::<ares_llm::ModelOverride>().is_some() {
            Arc::clone(ctx)
        } else {
            ctx.with_intercept(ares_llm::ModelOverride {
                model: model_name.to_string(),
            })
        };
        let content = self
            .llm
            .complete(&request_ctx, &prompt)
            .await;
        let response = content.map_err(|e| format!("LLM generation failed: {e}")).map(
            |content| LLMResponse {
                content,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: None,
            },
        );
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
        let client = match self.llm.get_client(ctx, CapabilityRequirements::default()).await {
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
                    let result = Box::pin(self.execute_skill_at_depth(
                        &skill_id,
                        tenant_id,
                        input,
                        run_id,
                        ctx,
                        depth + 1,
                    ))
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
    ) -> Result<(), String> {
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
                let result = Box::pin(self.execute_skill_at_depth(
                    skill_id,
                    tenant_id,
                    input.clone(),
                    run_id,
                    ctx,
                    depth + 1,
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
        let estimated_cost_usd = estimated_cost_usd(
            usage.prompt_tokens as i64,
            usage.completion_tokens as i64,
        );
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
    fn init(
        &self,
        _ctx: &std::sync::Arc<cordis::Context>,
    ) -> cordis::ServiceInitFuture<'_> {
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
                &[("user".to_string(), "ignored if provider is reached".to_string())],
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
        let engine_tools = Tools::from_static([
            Arc::new(EventProbeTool(Arc::clone(&reached))) as Arc<dyn Tool>,
        ]);
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

    fn test_db_url() -> Option<String> {
        std::env::var("TEST_DATABASE_URL")
            .ok()
            .or_else(|| std::env::var("DATABASE_URL").ok())
    }

    async fn try_test_pool() -> Option<PgPool> {
        let pool = PgPool::connect(&test_db_url()?).await.ok()?;
        Some(pool)
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
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let err = ensure_tenant_tool_allowed(&pool, &tenant_id, "calendar")
            .await
            .expect_err("missing allowlist row should deny");
        assert!(err.contains("calendar"));
    }

    #[tokio::test]
    async fn ensure_tenant_tool_allowed_accepts_enabled_row() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
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
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let tenant_id = format!("tenant-{}", uuid::Uuid::new_v4());
        let err = ensure_tenant_model_allowed(&pool, &tenant_id, "gpt-4o")
            .await
            .expect_err("missing model allowlist row should deny");
        assert!(err.contains("gpt-4o"));
    }

    #[tokio::test]
    async fn ensure_tenant_model_allowed_accepts_enabled_row() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
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
        assert_eq!(
            estimated_cost_usd(0, 0),
            rust_decimal::Decimal::ZERO
        );
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
                AmbientBehavior::Fail(message) => {
                    Err(AppError::Internal(message.clone()))
                }
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
        ) -> ares_types::Result<Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>>
        {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> ares_types::Result<Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>>
        {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> ares_types::Result<Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>>
        {
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
            response.expect("completion must succeed despite enrichment failure").content,
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
}
