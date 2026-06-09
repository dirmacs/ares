//! Skill execution engine — turns a Skill definition into an agent run.

use crate::db::run_history::{LogLlmCallRequest, LogToolCallRequest, RunHistoryStore};
use crate::db::skills::SkillStore;
use sqlx::PgPool;
use tracing;

/// One step inside a skill workflow.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)]
pub enum SkillStep {
    /// Call a tool by name with JSON arguments.
    #[serde(rename = "tool_call")]
    ToolCall {
        /// Name of the tool to invoke.
        tool_name: String,
        /// Arguments passed to the tool.
        args: serde_json::Value,
    },
    /// Call an LLM with a prompt and a model tier.
    #[serde(rename = "llm_call")]
    LlmCall {
        /// Prompt text sent to the model.
        prompt: String,
        /// Abstract model tier (e.g. "fast", "quality").
        model_tier: String,
    },
    /// Conditional branch evaluated against execution context.
    #[serde(rename = "condition")]
    Condition {
        /// Expression string to evaluate.
        expression: String,
        /// Steps to execute when the condition is true.
        then_steps: Vec<SkillStep>,
    },
}

/// Engine that loads a [`Skill`] from the DB and executes its steps.
pub struct SkillEngine {
    pool: PgPool,
}

impl SkillEngine {
    /// Create a new engine bound to the given pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Execute a skill by id for a tenant.
    ///
    /// Steps are run sequentially. Tool calls and LLM calls are logged to
    /// `run_history` as "pending" so the execution trace is visible even
    /// before the actual runtime integrations are wired.
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
                    let _ = self
                        .log_tool_call(run_id, tenant_id, step_index, &tool_name, args.clone())
                        .await;
                    context[format!("step_{}", step_index)] = serde_json::json!({
                        "tool": tool_name,
                        "status": "not_implemented"
                    });
                }
                SkillStep::LlmCall { prompt, model_tier } => {
                    tracing::info!("Step {}: llm_call (tier: {})", step_index, model_tier);
                    let _ = self
                        .log_llm_call(run_id, tenant_id, step_index, model_tier)
                        .await;
                    context[format!("step_{}", step_index)] = serde_json::json!({
                        "prompt": prompt,
                        "status": "not_implemented"
                    });
                }
                SkillStep::Condition {
                    expression,
                    then_steps,
                } => {
                    tracing::info!("Step {}: condition {}", step_index, expression);
                    // In full impl: evaluate expression against context
                    // For now, skip conditionally
                    let _ = then_steps;
                }
            }
            step_index += 1;
        }

        Ok(context)
    }

    async fn log_tool_call(
        &self,
        run_id: &str,
        tenant_id: &str,
        step_index: i32,
        tool_name: &str,
        args: serde_json::Value,
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
            result: None,
            latency_ms: 0,
            status: "pending".to_string(),
            error_message: None,
            created_at: chrono::Utc::now().timestamp(),
        };
        store
            .insert_tool_call(&req)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn log_llm_call(
        &self,
        run_id: &str,
        tenant_id: &str,
        step_index: i32,
        model_tier: String,
    ) -> Result<(), String> {
        let store = RunHistoryStore::new(&self.pool);
        let req = LogLlmCallRequest {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            tenant_id: tenant_id.to_string(),
            agent_name: "skill_executor".to_string(),
            step_index,
            provider: "skill".to_string(),
            model: model_tier,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            estimated_cost_usd: rust_decimal::Decimal::ZERO,
            latency_ms: 0,
            status: "pending".to_string(),
            error_message: None,
            request_payload: None,
            response_payload: None,
            created_at: chrono::Utc::now().timestamp(),
        };
        store
            .insert_llm_call(&req)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
