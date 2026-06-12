//! Skill execution engine — turns a Skill definition into an agent run.

use crate::db::run_history::{LogLlmCallRequest, LogToolCallRequest, RunHistoryStore};
use crate::db::skills::SkillStore;
use crate::{AresConfigManager, ConfigBasedLLMFactory, RuntimeToolRegistry, ToolRegistry};
use ares_llm::{LLMClient, LLMResponse};
use sqlx::PgPool;
use std::sync::Arc;

/// One step inside a skill workflow.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)]
pub enum SkillStep {
    /// Call a tool by name with JSON arguments.
    #[serde(rename = "tool_call")]
    ToolCall {
        tool_name: String,
        args: serde_json::Value,
    },
    /// Call an LLM with a prompt and a model tier.
    #[serde(rename = "llm_call")]
    LlmCall {
        prompt: String,
        model_tier: String,
    },
    /// Conditional branch evaluated against execution context.
    #[serde(rename = "condition")]
    Condition {
        expression: String,
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
        // 1. Load the skill definition
        let skill_store = SkillStore::new(&self.pool);
        let skill = skill_store
            .get_skill(skill_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Skill not found".to_string())?;

        // 2. Parse steps JSONB into SkillStep vec
        let steps: Vec<SkillStep> =
            serde_json::from_value(skill.steps).map_err(|e| format!("Invalid skill steps: {}", e))?;

        // 3. Execute each step sequentially
        let mut context = serde_json::json!({"input": input});
        let mut step_index: i32 = 0;

        for step in steps {
            match step {
                SkillStep::ToolCall { tool_name, args } => {
                    tracing::info!("Step {}: tool_call {}", step_index, tool_name);
                    let start = std::time::Instant::now();

                    // Try runtime registry first, then static registry
                    let result = if let Ok(rt) = self
                        .runtime_tool_registry
                        .execute_for_tenant(&tool_name, args.clone(), Some(tenant_id))
                        .await
                    {
                        rt
                    } else if let Some(tool) = self.tool_registry.get(&tool_name) {
                        let tool = Arc::clone(tool);
                        tool.execute(args.clone())
                            .await
                            .map_err(|e| format!("Tool execution error: {}", e))?
                    } else {
                        return Err(format!("Tool {} not found", tool_name));
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

                    let response = client
                        .generate_with_history(&messages)
                        .await
                        .map_err(|e| format!("LLM generation failed: {}", e))?;

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
                SkillStep::Condition {
                    expression,
                    then_steps,
                } => {
                    tracing::info!("Step {}: condition {}", step_index, expression);
                    let condition_met = evaluate_condition(&expression, &context);
                    if condition_met {
                        // Recursively execute then_steps
                        for (sub_idx, sub_step) in then_steps.iter().enumerate() {
                            let sub_step_index = step_index + 1 + sub_idx as i32;
                            self.execute_sub_step(
                                sub_step,
                                tenant_id,
                                run_id,
                                sub_step_index,
                                &mut context,
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
    ) -> Result<(), String> {
        match step {
            SkillStep::ToolCall { tool_name, args } => {
                tracing::info!("Sub-step {}: tool_call {}", step_index, tool_name);
                let start = std::time::Instant::now();

                let result = if let Ok(rt) = self
                    .runtime_tool_registry
                    .execute_for_tenant(tool_name, args.clone(), Some(tenant_id))
                    .await
                {
                    rt
                } else if let Some(tool) = self.tool_registry.get(tool_name) {
                    let tool = Arc::clone(tool);
                    tool.execute(args.clone())
                        .await
                        .map_err(|e| format!("Tool execution error: {}", e))?
                } else {
                    return Err(format!("Tool {} not found", tool_name));
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

                let messages = vec![("user".to_string(), prompt.clone())];
                let registry = self.llm_factory.registry();
                let client = match registry.create_client_for_model(&model_name).await {
                    Ok(c) => c,
                    Err(_) => registry
                        .create_client_for_provider(&provider_name)
                        .await
                        .map_err(|e| format!("LLM client creation failed: {}", e))?,
                };

                let response = client
                    .generate_with_history(&messages)
                    .await
                    .map_err(|e| format!("LLM generation failed: {}", e))?;

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
            SkillStep::Condition {
                expression,
                then_steps,
            } => {
                tracing::info!("Sub-step {}: condition {}", step_index, expression);
                let condition_met = evaluate_condition(expression, context);
                if condition_met {
                    for (sub_idx, sub_step) in then_steps.iter().enumerate() {
                        let sub_step_index = step_index + 1 + sub_idx as i32;
                        Box::pin(self.execute_sub_step(
                            sub_step,
                            tenant_id,
                            run_id,
                            sub_step_index,
                            context,
                        ))
                        .await?;
                    }
                }
            }
        }
        Ok(())
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
            status: "completed".to_string(),
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
            status: "completed".to_string(),
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
