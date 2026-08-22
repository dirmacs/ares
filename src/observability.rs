//! Run observability implementation for ARES.
//!
//! Bridges `ares-llm`'s `ObservabilitySink` trait to `ares-store`'s
//! `RunHistoryStore`, enabling LLM/tool call logging, budget checks,
//! and run cost aggregation.

use ares_store::run_history::{
    BudgetAlert, LogLlmCallRequest, LogToolCallRequest, RunCost, RunHistoryStore,
};
use ares_llm::observability::{LlmCallRecord, ObservabilitySink, ToolCallRecord};
use chrono::Datelike;
use rust_decimal::Decimal;
#[cfg(feature = "postgres")]
use sqlx::PgPool;
#[cfg(not(feature = "postgres"))]
type PgPool = ();
use uuid::Uuid;

/// Rough blended cost estimate: $0.002 per 1K tokens.
pub(crate) fn estimated_cost_usd(prompt_tokens: i64, completion_tokens: i64) -> Decimal {
    let total = prompt_tokens + completion_tokens;
    // $0.002 / 1K tokens  =>  total * 2 / 1_000_000
    Decimal::new(total * 2, 6)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunCostAggregationRequest {
    pub(crate) run_id: String,
    pub(crate) tenant_id: String,
    pub(crate) agent_name: String,
    pub(crate) duration_ms: i64,
}

pub(crate) fn run_cost_aggregation_request(
    run_id: &str,
    tenant_id: &str,
    agent_name: &str,
    duration_ms: i64,
) -> RunCostAggregationRequest {
    RunCostAggregationRequest {
        run_id: run_id.to_string(),
        tenant_id: tenant_id.to_string(),
        agent_name: agent_name.to_string(),
        duration_ms,
    }
}

pub(crate) fn spawn_run_cost_aggregation(pool: PgPool, request: RunCostAggregationRequest) {
    let obs = RunObservability {
        run_id: request.run_id,
        tenant_id: request.tenant_id,
        agent_name: request.agent_name,
        pool,
    };
    tokio::spawn(async move {
        obs.aggregate_run_cost(request.duration_ms).await;
    });
}

/// Concrete observability sink that writes to PostgreSQL run history tables.
pub struct RunObservability {
    /// Unique run identifier.
    pub run_id: String,
    /// Tenant identifier.
    pub tenant_id: String,
    /// Agent name.
    pub agent_name: String,
    /// PostgreSQL connection pool.
    pub pool: PgPool,
}

#[async_trait::async_trait]
impl ObservabilitySink for RunObservability {
    async fn log_llm_call(&self, record: LlmCallRecord) {
        let estimated = estimated_cost_usd(record.prompt_tokens, record.completion_tokens);

        // Budget check (best-effort; never fail the run)
        if let Err(e) = self.check_budget(estimated).await {
            tracing::warn!(error = %e, run_id = %self.run_id, "Budget check failed");
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_cost_aggregation_request_preserves_run_identity() {
        let req = run_cost_aggregation_request("run-1", "tenant-a", "agent-a", 42);
        assert_eq!(req.run_id, "run-1");
        assert_eq!(req.tenant_id, "tenant-a");
        assert_eq!(req.agent_name, "agent-a");
        assert_eq!(req.duration_ms, 42);
    }
}

#[cfg(feature = "postgres")]
impl RunObservability {
    /// Check whether the tenant's budget would be exceeded by the estimated cost.
    /// This is informational only — the LLM call has already been made.
    pub async fn check_budget(&self, estimated_cost: Decimal) -> std::result::Result<(), String> {
        let store = RunHistoryStore::new(&self.pool);
        let budget = match store.get_tenant_budget(&self.tenant_id).await {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(()), // No budget configured
            Err(e) => {
                tracing::warn!(error = %e, "Failed to fetch tenant budget");
                return Ok(());
            }
        };

        // Sum existing costs for this tenant in the current month
        let now = chrono::Utc::now();
        let period_start = now
            .date_naive()
            .with_day(1)
            .unwrap_or(now.date_naive())
            .and_time(chrono::NaiveTime::MIN)
            .and_utc()
            .timestamp();
        let period_end = now.timestamp();

        let spent = match sqlx::query_scalar::<_, Decimal>(
            "SELECT COALESCE(SUM(total_estimated_cost_usd), 0) FROM run_costs \
             WHERE tenant_id = $1 AND created_at >= $2 AND created_at <= $3",
        )
        .bind(&self.tenant_id)
        .bind(period_start)
        .bind(period_end)
        .fetch_one(&self.pool)
        .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to compute spend");
                return Ok(());
            }
        };

        if spent + estimated_cost > budget.monthly_limit_usd {
            tracing::warn!(
                tenant_id = %self.tenant_id,
                spent = %spent,
                budget = %budget.monthly_limit_usd,
                estimated = %estimated_cost,
                "Tenant budget would be exceeded"
            );

            // Insert a budget alert
            let alert = BudgetAlert {
                id: Uuid::new_v4().to_string(),
                tenant_id: self.tenant_id.clone(),
                alert_type: "monthly_limit".to_string(),
                current_spend_usd: spent,
                limit_usd: budget.monthly_limit_usd,
                threshold_pct: budget.alert_threshold_pct,
                period_start,
                period_end,
                acknowledged: false,
                acknowledged_by: None,
                acknowledged_at: None,
                created_at: now.timestamp(),
            };

            if let Err(e) = store.insert_budget_alert(&alert).await {
                tracing::warn!(error = %e, "Failed to insert budget alert");
            }

            return Err("Tenant monthly budget exceeded".to_string());
        }

        Ok(())
    }

    /// Aggregate all LLM and tool calls for this run into `run_costs`.
    pub async fn aggregate_run_cost(&self, duration_ms: i64) {
        let store = RunHistoryStore::new(&self.pool);

        let llm_calls = match store.get_llm_calls_for_run(&self.run_id).await {
            Ok(calls) => calls,
            Err(e) => {
                tracing::warn!(error = %e, run_id = %self.run_id, "Failed to fetch LLM calls for cost aggregation");
                return;
            }
        };

        let tool_calls = match store.get_tool_calls_for_run(&self.run_id).await {
            Ok(calls) => calls,
            Err(e) => {
                tracing::warn!(error = %e, run_id = %self.run_id, "Failed to fetch tool calls for cost aggregation");
                return;
            }
        };

        let total_prompt_tokens: i64 = llm_calls.iter().map(|c| c.prompt_tokens).sum();
        let total_completion_tokens: i64 = llm_calls.iter().map(|c| c.completion_tokens).sum();
        let total_estimated_cost: Decimal = llm_calls.iter().map(|c| c.estimated_cost_usd).sum();

        let record = RunCost {
            run_id: self.run_id.clone(),
            tenant_id: self.tenant_id.clone(),
            agent_name: self.agent_name.clone(),
            total_llm_calls: llm_calls.len() as i32,
            total_tool_calls: tool_calls.len() as i32,
            total_prompt_tokens,
            total_completion_tokens,
            total_estimated_cost_usd: total_estimated_cost,
            total_duration_ms: duration_ms,
            created_at: chrono::Utc::now().timestamp(),
        };

        if let Err(e) = store.upsert_run_cost(&record).await {
            tracing::warn!(error = %e, run_id = %self.run_id, "Failed to upsert run cost");
        }
    }
}
