//! Per-run observability sink: `run_llm_calls` / `run_tool_calls` via `ares-store`.
//!
//! HTTP handlers use `ares-http::observability::RunObservability`; trigger/scheduler
//! paths in this crate need the same wiring without depending on `ares-http`.

use ares_llm::observability::{LlmCallRecord, ObservabilitySink, ToolCallRecord};
use ares_store::run_history::{LogLlmCallRequest, LogToolCallRequest, RunHistoryStore};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

fn estimated_cost_usd(prompt_tokens: i64, completion_tokens: i64) -> Decimal {
    let total = prompt_tokens + completion_tokens;
    Decimal::new(total * 2, 6)
}

/// Writes LLM/tool steps for one run into run history (same shape as the HTTP sink).
pub struct RunHistoryObservability {
    pub pool: PgPool,
    pub run_id: String,
    pub tenant_id: String,
    pub agent_name: String,
}

#[async_trait::async_trait]
impl ObservabilitySink for RunHistoryObservability {
    async fn log_llm_call(&self, record: LlmCallRecord) {
        let estimated = estimated_cost_usd(record.prompt_tokens, record.completion_tokens);
        let req = LogLlmCallRequest {
            id: Uuid::new_v4().to_string(),
            run_id: self.run_id.clone(),
            tenant_id: self.tenant_id.clone(),
            agent_name: self.agent_name.clone(),
            step_index: record.step_index,
            provider: record.provider,
            model: record.model,
            prompt_tokens: record.prompt_tokens,
            completion_tokens: record.completion_tokens,
            total_tokens: record.prompt_tokens + record.completion_tokens,
            estimated_cost_usd: estimated,
            latency_ms: record.latency_ms,
            cached_tokens: record.cached_tokens,
            total_time_ms: record.total_time_ms,
            status: record.status,
            error_message: None,
            request_payload: None,
            response_payload: None,
            created_at: chrono::Utc::now().timestamp(),
        };
        let store = RunHistoryStore::new(&self.pool);
        if let Err(e) = store.insert_llm_call(&req).await {
            tracing::warn!(error = %e, run_id = %self.run_id, "Failed to log LLM call");
        }
    }

    async fn log_tool_call(&self, record: ToolCallRecord) {
        let req = LogToolCallRequest {
            id: Uuid::new_v4().to_string(),
            run_id: self.run_id.clone(),
            tenant_id: self.tenant_id.clone(),
            agent_name: self.agent_name.clone(),
            step_index: record.step_index,
            tool_name: record.tool_name,
            tool_type: record.tool_type,
            arguments: record.arguments,
            result: record.result,
            latency_ms: record.latency_ms,
            status: record.status,
            error_message: None,
            created_at: chrono::Utc::now().timestamp(),
        };
        let store = RunHistoryStore::new(&self.pool);
        if let Err(e) = store.insert_tool_call(&req).await {
            tracing::warn!(error = %e, run_id = %self.run_id, "Failed to log tool call");
        }
    }
}
