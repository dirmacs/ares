//! Skill execution engine — turns a Skill definition into an agent run.

use crate::db::run_history::{LogLlmCallRequest, LogToolCallRequest, RunHistoryStore};
use crate::db::skills::SkillStore;
use crate::db::tenant_allowlist::TenantAllowlistStore;
use crate::{AresConfigManager, ConfigBasedLLMFactory, RuntimeToolRegistry, ToolRegistry};
use ares_llm::LLMResponse;
use ares_types::AppError;
use sqlx::PgPool;
use std::sync::Arc;

const MAX_SKILL_CALL_DEPTH: usize = 8;
const RUN_HISTORY_STATUS_SUCCESS: &str = "success";

fn default_step_input() -> serde_json::Value {
    serde_json::Value::Null
}

pub(crate) fn skill_result_token_counts(result: &serde_json::Value) -> (i64, i64) {
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
    tool_registry: Arc<ToolRegistry>,
    runtime_tool_registry: Arc<RuntimeToolRegistry>,
    llm_factory: Arc<ConfigBasedLLMFactory>,
    config_manager: Arc<AresConfigManager>,
}

impl SkillEngine {
    /// Create a new engine with all runtime dependencies.
    pub fn new(
        pool: PgPool,
        tool_registry: Arc<ToolRegistry>,
        runtime_tool_registry: Arc<RuntimeToolRegistry>,
        llm_factory: Arc<ConfigBasedLLMFactory>,
        config_manager: Arc<AresConfigManager>,
    ) -> Self {
        Self {
            pool,
            tool_registry,
            runtime_tool_registry,
            llm_factory,
            config_manager,
        }
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
    ) -> Result<serde_json::Value, String> {
        self.execute_skill_at_depth(skill_id, tenant_id, input, run_id, 0)
            .await
    }

    async fn execute_skill_at_depth(
        &self,
        skill_id: &str,
        tenant_id: &str,
        input: serde_json::Value,
        run_id: &str,
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

                    // Try runtime registry first; fall back to static registry only when
                    // no tenant-visible runtime tool exists. Runtime execution failures
                    // must surface instead of silently invoking a different built-in tool
                    // with the same name.
                    let result = match self
                        .runtime_tool_registry
                        .execute_for_tenant(&tool_name, args.clone(), Some(tenant_id))
                        .await
                    {
                        Ok(rt) => rt,
                        Err(err) if runtime_tool_error_allows_static_fallback(&err) => {
                            if let Some(tool) = self.tool_registry.get(&tool_name) {
                                let tool = Arc::clone(tool);
                                tool.execute(args.clone())
                                    .await
                                    .map_err(|e| format!("Tool execution error: {}", e))?
                            } else {
                                return Err(format!("Tool {} not found", tool_name));
                            }
                        }
                        Err(err) => {
                            return Err(format!(
                                "Runtime tool {} execution error: {}",
                                tool_name, err
                            ));
                        }
                    };

                    let latency_ms = start.elapsed().as_millis() as i64;

                    // Store result in context
                    context[&format!("step_{}", step_index)] = result.clone();

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
                    let config = self.config_manager.config();
                    let (provider_name, model_name) =
                        super::resolve_model_tier(tenant_id, &model_tier, &self.pool, &config)
                            .await
                            .unwrap_or_else(|| ("default".to_string(), model_tier.clone()));
                    ensure_tenant_model_allowed(&self.pool, tenant_id, &model_name).await?;

                    // Build messages and call LLM
                    let messages = vec![("user".to_string(), prompt.clone())];
                    let registry = self.llm_factory.registry();
                    let client = match registry.create_client_for_model(&model_name).await {
                        Ok(c) => c,
                        Err(_) => registry
                            .create_client_for_provider(&provider_name)
                            .await
                            .map_err(|e| format!("LLM client creation failed: {}", e))?,
                    };

                    self.enforce_token_budget_before_llm_call(tenant_id).await?;
                    let response = client
                        .generate_with_history(&messages)
                        .await
                        .map_err(|e| format!("LLM generation failed: {}", e))?;
                    self.record_llm_token_budget_usage(tenant_id, run_id, &model_name, &response)
                        .await?;

                    let latency_ms = start.elapsed().as_millis() as i64;

                    // Store result
                    let result = serde_json::json!({
                        "content": response.content,
                        "usage": response.usage,
                    });
                    context[&format!("step_{}", step_index)] = result.clone();

                    // Log
                    self.log_llm_call_result(
                        run_id,
                        tenant_id,
                        step_index,
                        &provider_name,
                        &model_name,
                        response,
                        latency_ms,
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
                        depth + 1,
                    ))
                    .await?;
                    context[&format!("step_{}", step_index)] = result;
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

                let result = match self
                    .runtime_tool_registry
                    .execute_for_tenant(tool_name, args.clone(), Some(tenant_id))
                    .await
                {
                    Ok(rt) => rt,
                    Err(err) if runtime_tool_error_allows_static_fallback(&err) => {
                        if let Some(tool) = self.tool_registry.get(tool_name) {
                            let tool = Arc::clone(tool);
                            tool.execute(args.clone())
                                .await
                                .map_err(|e| format!("Tool execution error: {}", e))?
                        } else {
                            return Err(format!("Tool {} not found", tool_name));
                        }
                    }
                    Err(err) => {
                        return Err(format!(
                            "Runtime tool {} execution error: {}",
                            tool_name, err
                        ));
                    }
                };

                let latency_ms = start.elapsed().as_millis() as i64;
                context[&format!("step_{}", step_index)] = result.clone();

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

                let config = self.config_manager.config();
                let (provider_name, model_name) =
                    super::resolve_model_tier(tenant_id, model_tier, &self.pool, &config)
                        .await
                        .unwrap_or_else(|| ("default".to_string(), model_tier.clone()));
                ensure_tenant_model_allowed(&self.pool, tenant_id, &model_name).await?;

                let messages = vec![("user".to_string(), prompt.clone())];
                let registry = self.llm_factory.registry();
                let client = match registry.create_client_for_model(&model_name).await {
                    Ok(c) => c,
                    Err(_) => registry
                        .create_client_for_provider(&provider_name)
                        .await
                        .map_err(|e| format!("LLM client creation failed: {}", e))?,
                };

                self.enforce_token_budget_before_llm_call(tenant_id).await?;
                let response = client
                    .generate_with_history(&messages)
                    .await
                    .map_err(|e| format!("LLM generation failed: {}", e))?;
                self.record_llm_token_budget_usage(tenant_id, run_id, &model_name, &response)
                    .await?;

                let latency_ms = start.elapsed().as_millis() as i64;
                let result = serde_json::json!({
                    "content": response.content,
                    "usage": response.usage,
                });
                context[&format!("step_{}", step_index)] = result.clone();

                self.log_llm_call_result(
                    run_id,
                    tenant_id,
                    step_index,
                    &provider_name,
                    &model_name,
                    response,
                    latency_ms,
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
                    depth + 1,
                ))
                .await?;
                context[&format!("step_{}", step_index)] = result;
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
        let store = ares_db::token_budgets::TokenBudgetStore::new(&self.pool);
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
        let store = ares_db::token_budgets::TokenBudgetStore::new(&self.pool);
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

    async fn log_llm_call_result(
        &self,
        run_id: &str,
        tenant_id: &str,
        step_index: i32,
        provider: &str,
        model: &str,
        response: LLMResponse,
        latency_ms: i64,
    ) -> Result<(), String> {
        let store = RunHistoryStore::new(&self.pool);
        let usage = response.usage.unwrap_or_default();
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
            estimated_cost_usd: rust_decimal::Decimal::ZERO,
            latency_ms: latency_ms,
            status: RUN_HISTORY_STATUS_SUCCESS.to_string(),
            error_message: None,
            request_payload: None,
            response_payload: Some(serde_json::json!({
                "content": response.content,
                "finish_reason": response.finish_reason,
            })),
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
        Ok(false) => Err(format!(
            "Model '{}' is not allowed for tenant '{}'",
            model_name, tenant_id
        )),
        Err(e) => Err(format!("Failed to check model allowlist: {}", e)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
