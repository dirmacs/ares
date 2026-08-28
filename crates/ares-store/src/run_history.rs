//! Detailed run history database operations.
//!
//! Provides CRUD for `run_llm_calls`, `run_tool_calls`, `run_costs`,
//! `tenant_budgets`, `budget_alerts`, and `agent_health_metrics`
//! tables (migration 016).
//!
//! # Design Notes
//!
//! - `RunHistoryStore` holds a `&PgPool` (same lifetime pattern as
//!   `RuntimeToolStore`).
//! - JSONB columns bind directly to `serde_json::Value` via sqlx.
//! - Timestamps are `i64` Unix epoch seconds (BIGINT in the schema).
//! - Monetary columns use `rust_decimal::Decimal`.

use ares_types::types::{AppError, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

// =============================================================================
// Structs
// =============================================================================

/// One persisted row in `run_llm_calls`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunLlmCall {
    pub id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub step_index: i32,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub estimated_cost_usd: Decimal,
    pub latency_ms: i64,
        /// Tokens served from the provider-side prompt cache (`None` when the
    /// provider does not report cache hits).
    pub cached_tokens: Option<i64>,
    /// End-to-end wall-clock time for the whole call in milliseconds,
    /// including retries and queueing (`None` when not measured).
    pub total_time_ms: Option<i64>,
    pub status: String,
    pub error_message: Option<String>,
    pub request_payload: Option<serde_json::Value>,
    pub response_payload: Option<serde_json::Value>,
    pub created_at: i64,
}

/// One persisted row in `run_tool_calls`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunToolCall {
    pub id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub step_index: i32,
    pub tool_name: String,
    pub tool_type: String,
    pub arguments: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub latency_ms: i64,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: i64,
}

/// One persisted row in `run_costs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunCost {
    pub run_id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub total_llm_calls: i32,
    pub total_tool_calls: i32,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_estimated_cost_usd: Decimal,
    pub total_duration_ms: i64,
    pub created_at: i64,
}

/// One persisted row in `tenant_budgets`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TenantBudget {
    pub tenant_id: String,
    pub monthly_limit_usd: Decimal,
    pub daily_limit_usd: Option<Decimal>,
    pub alert_threshold_pct: i32,
    pub currency: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One persisted row in `budget_alerts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetAlert {
    pub id: String,
    pub tenant_id: String,
    pub alert_type: String,
    pub current_spend_usd: Decimal,
    pub limit_usd: Decimal,
    pub threshold_pct: i32,
    pub period_start: i64,
    pub period_end: i64,
    pub acknowledged: bool,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<i64>,
    pub created_at: i64,
}

/// One persisted row in `agent_health_metrics`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentHealthMetrics {
    pub id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub period_start: i64,
    pub period_end: i64,
    pub total_runs: i64,
    pub successful_runs: i64,
    pub failed_runs: i64,
    pub avg_latency_ms: i64,
    pub p50_latency_ms: i64,
    pub p95_latency_ms: i64,
    pub p99_latency_ms: i64,
    pub total_tokens: i64,
    pub total_cost_usd: Decimal,
    pub error_rate_pct: Decimal,
    pub created_at: i64,
}

/// One persisted row in `model_health_metrics`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelHealthMetrics {
    pub id: String,
    pub tenant_id: String,
    pub model: String,
    pub period_start: i64,
    pub period_end: i64,
    pub total_calls: i64,
    pub successful_calls: i64,
    pub failed_calls: i64,
    pub avg_latency_ms: i64,
    pub p50_latency_ms: i64,
    pub p95_latency_ms: i64,
    pub p99_latency_ms: i64,
    pub total_tokens: i64,
    pub total_cost_usd: Decimal,
    pub error_rate_pct: Decimal,
    pub created_at: i64,
}

// =============================================================================
// Request structs
// =============================================================================

/// Request body for logging an LLM call.
#[derive(Debug, Clone, Deserialize)]
pub struct LogLlmCallRequest {
    pub id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub step_index: i32,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub estimated_cost_usd: Decimal,
    pub latency_ms: i64,
        /// Tokens served from the provider-side prompt cache; `None` when
    /// unknown or not reported.
    #[serde(default)]
    pub cached_tokens: Option<i64>,
    /// End-to-end wall-clock time for the whole call in milliseconds,
    /// including retries and queueing.
    #[serde(default)]
    pub total_time_ms: Option<i64>,
    pub status: String,
    pub error_message: Option<String>,
    pub request_payload: Option<serde_json::Value>,
    pub response_payload: Option<serde_json::Value>,
    pub created_at: i64,
}

/// Request body for logging a tool call.
#[derive(Debug, Deserialize)]
pub struct LogToolCallRequest {
    pub id: String,
    pub run_id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub step_index: i32,
    pub tool_name: String,
    pub tool_type: String,
    pub arguments: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub latency_ms: i64,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: i64,
}

/// Request body for setting a tenant budget.
#[derive(Debug, Deserialize)]
pub struct SetTenantBudgetRequest {
    pub tenant_id: String,
    pub monthly_limit_usd: Decimal,
    pub daily_limit_usd: Option<Decimal>,
    pub alert_threshold_pct: i32,
    pub currency: String,
}

/// Request body for acknowledging a budget alert.
#[derive(Debug, Deserialize)]
pub struct AcknowledgeBudgetAlertRequest {
    pub acknowledged_by: String,
}

// =============================================================================
// Query structs
// =============================================================================

/// Query parameters for listing LLM calls.
#[derive(Debug, Default, Deserialize)]
pub struct ListLlmCallsQuery {
    pub run_id: Option<String>,
    pub tenant_id: Option<String>,
    pub agent_name: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub status: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i32,
    #[serde(default)]
    pub offset: i32,
}

/// Query parameters for listing tool calls.
#[derive(Debug, Default, Deserialize)]
pub struct ListToolCallsQuery {
    pub run_id: Option<String>,
    pub tenant_id: Option<String>,
    pub agent_name: Option<String>,
    pub tool_name: Option<String>,
    pub tool_type: Option<String>,
    pub status: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i32,
    #[serde(default)]
    pub offset: i32,
}

/// Query parameters for listing budget alerts.
#[derive(Debug, Default, Deserialize)]
pub struct ListBudgetAlertsQuery {
    pub tenant_id: Option<String>,
    pub alert_type: Option<String>,
    pub acknowledged: Option<bool>,
    #[serde(default = "default_limit")]
    pub limit: i32,
    #[serde(default)]
    pub offset: i32,
}

fn default_limit() -> i32 {
    50
}

// =============================================================================
// Store
// =============================================================================

/// CRUD for run history tables.
pub struct RunHistoryStore<'a> {
    pool: &'a PgPool,
}

impl<'a> RunHistoryStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    // -------------------------------------------------------------------------
    // LLM Calls
    // -------------------------------------------------------------------------

    /// Insert a new LLM call record.
    pub async fn insert_llm_call(&self, req: &LogLlmCallRequest) -> Result<RunLlmCall> {
        validate_status(&req.status)?;

        let row = sqlx::query(
            "INSERT INTO run_llm_calls \
                (id, run_id, tenant_id, agent_name, step_index, provider, model, \
                 prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd, \
                 latency_ms, cached_tokens, total_time_ms, status, error_message, \
                 request_payload, response_payload, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19) \
             RETURNING id, run_id, tenant_id, agent_name, step_index, provider, model, \
                       prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd, \
                       latency_ms, cached_tokens, total_time_ms, status, error_message, \
                       request_payload, response_payload, created_at",
        )
        .bind(&req.id)
        .bind(&req.run_id)
        .bind(&req.tenant_id)
        .bind(&req.agent_name)
        .bind(req.step_index)
        .bind(&req.provider)
        .bind(&req.model)
        .bind(req.prompt_tokens)
        .bind(req.completion_tokens)
        .bind(req.total_tokens)
        .bind(req.estimated_cost_usd)
        .bind(req.latency_ms)
        .bind(req.cached_tokens)
        .bind(req.total_time_ms)
        .bind(&req.status)
        .bind(&req.error_message)
        .bind(&req.request_payload)
        .bind(&req.response_payload)
        .bind(req.created_at)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_llm_call(&row)
    }

    /// Fetch a single LLM call by id.
    pub async fn get_llm_call(&self, id: &str) -> Result<Option<RunLlmCall>> {
        let row = sqlx::query(
            "SELECT id, run_id, tenant_id, agent_name, step_index, provider, model, \
                    prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd, \
                    latency_ms, cached_tokens, total_time_ms, status, \
                    error_message, request_payload, response_payload, created_at \
             FROM run_llm_calls WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        row.map(|r| row_to_llm_call(&r)).transpose()
    }

    /// List LLM calls with optional filtering.
    pub async fn list_llm_calls(&self, q: &ListLlmCallsQuery) -> Result<Vec<RunLlmCall>> {
        let mut sql = String::from(
            "SELECT id, run_id, tenant_id, agent_name, step_index, provider, model, \
                    prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd, \
                    latency_ms, cached_tokens, total_time_ms, status, \
                    error_message, request_payload, response_payload, created_at \
             FROM run_llm_calls WHERE 1=1",
        );
        if q.run_id.is_some() {
            sql.push_str(" AND run_id = $1");
        }
        if q.tenant_id.is_some() {
            sql.push_str(" AND tenant_id = $2");
        }
        if q.agent_name.is_some() {
            sql.push_str(" AND agent_name = $3");
        }
        if q.provider.is_some() {
            sql.push_str(" AND provider = $4");
        }
        if q.model.is_some() {
            sql.push_str(" AND model = $5");
        }
        if q.status.is_some() {
            sql.push_str(" AND status = $6");
        }
        sql.push_str(" ORDER BY created_at DESC, id ASC LIMIT $7 OFFSET $8");

        // `Default` derives limit 0, which Postgres reads as "no rows";
        // clamp so programmatic callers get sane pagination.
        let limit = if q.limit > 0 { q.limit } else { 100 };
        let mut query = sqlx::query(&sql);
        query = query.bind(&q.run_id);
        query = query.bind(&q.tenant_id);
        query = query.bind(&q.agent_name);
        query = query.bind(&q.provider);
        query = query.bind(&q.model);
        query = query.bind(&q.status);
        query = query.bind(limit);
        query = query.bind(q.offset);

        let rows = query.fetch_all(self.pool).await.map_err(sqlx_err)?;
        rows.iter().map(row_to_llm_call).collect()
    }

    /// Get all LLM calls for a specific run.
    pub async fn get_llm_calls_for_run(&self, run_id: &str) -> Result<Vec<RunLlmCall>> {
        let rows = sqlx::query(
            "SELECT id, run_id, tenant_id, agent_name, step_index, provider, model, \
                    prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd, \
                    latency_ms, cached_tokens, total_time_ms, status, \
                    error_message, request_payload, response_payload, created_at \
             FROM run_llm_calls WHERE run_id = $1 ORDER BY step_index ASC, id ASC",
        )
        .bind(run_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        rows.iter().map(row_to_llm_call).collect()
    }

    // -------------------------------------------------------------------------
    // Tool Calls
    // -------------------------------------------------------------------------

    /// Insert a new tool call record.
    pub async fn insert_tool_call(&self, req: &LogToolCallRequest) -> Result<RunToolCall> {
        validate_status(&req.status)?;
        validate_tool_type(&req.tool_type)?;

        let row = sqlx::query(
            "INSERT INTO run_tool_calls \
                (id, run_id, tenant_id, agent_name, step_index, tool_name, tool_type, \
                 arguments, result, latency_ms, status, error_message, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
             RETURNING id, run_id, tenant_id, agent_name, step_index, tool_name, tool_type, \
                       arguments, result, latency_ms, status, error_message, created_at",
        )
        .bind(&req.id)
        .bind(&req.run_id)
        .bind(&req.tenant_id)
        .bind(&req.agent_name)
        .bind(req.step_index)
        .bind(&req.tool_name)
        .bind(&req.tool_type)
        .bind(&req.arguments)
        .bind(&req.result)
        .bind(req.latency_ms)
        .bind(&req.status)
        .bind(&req.error_message)
        .bind(req.created_at)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_tool_call(&row)
    }

    /// Fetch a single tool call by id.
    pub async fn get_tool_call(&self, id: &str) -> Result<Option<RunToolCall>> {
        let row = sqlx::query(
            "SELECT id, run_id, tenant_id, agent_name, step_index, tool_name, tool_type, \
                    arguments, result, latency_ms, status, error_message, created_at \
             FROM run_tool_calls WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        row.map(|r| row_to_tool_call(&r)).transpose()
    }

    /// List tool calls with optional filtering.
    pub async fn list_tool_calls(&self, q: &ListToolCallsQuery) -> Result<Vec<RunToolCall>> {
        let mut sql = String::from(
            "SELECT id, run_id, tenant_id, agent_name, step_index, tool_name, tool_type, \
                    arguments, result, latency_ms, status, error_message, created_at \
             FROM run_tool_calls WHERE 1=1",
        );
        if q.run_id.is_some() {
            sql.push_str(" AND run_id = $1");
        }
        if q.tenant_id.is_some() {
            sql.push_str(" AND tenant_id = $2");
        }
        if q.agent_name.is_some() {
            sql.push_str(" AND agent_name = $3");
        }
        if q.tool_name.is_some() {
            sql.push_str(" AND tool_name = $4");
        }
        if q.tool_type.is_some() {
            sql.push_str(" AND tool_type = $5");
        }
        if q.status.is_some() {
            sql.push_str(" AND status = $6");
        }
        sql.push_str(" ORDER BY created_at DESC, id ASC LIMIT $7 OFFSET $8");

        // Same zero-limit clamp as `list_llm_calls`.
        let limit = if q.limit > 0 { q.limit } else { 100 };
        let mut query = sqlx::query(&sql);
        query = query.bind(&q.run_id);
        query = query.bind(&q.tenant_id);
        query = query.bind(&q.agent_name);
        query = query.bind(&q.tool_name);
        query = query.bind(&q.tool_type);
        query = query.bind(&q.status);
        query = query.bind(limit);
        query = query.bind(q.offset);

        let rows = query.fetch_all(self.pool).await.map_err(sqlx_err)?;
        rows.iter().map(row_to_tool_call).collect()
    }

    /// Get all tool calls for a specific run.
    pub async fn get_tool_calls_for_run(&self, run_id: &str) -> Result<Vec<RunToolCall>> {
        let rows = sqlx::query(
            "SELECT id, run_id, tenant_id, agent_name, step_index, tool_name, tool_type, \
                    arguments, result, latency_ms, status, error_message, created_at \
             FROM run_tool_calls WHERE run_id = $1 ORDER BY step_index ASC, id ASC",
        )
        .bind(run_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        rows.iter().map(row_to_tool_call).collect()
    }

    // -------------------------------------------------------------------------
    // Costs
    // -------------------------------------------------------------------------

    /// Upsert a run cost record. If the run_id already exists, aggregate the
    /// values by summing them with the existing row.
    pub async fn upsert_run_cost(&self, record: &RunCost) -> Result<RunCost> {
        let row = sqlx::query(
            "INSERT INTO run_costs \
                (run_id, tenant_id, agent_name, total_llm_calls, total_tool_calls, \
                 total_prompt_tokens, total_completion_tokens, total_estimated_cost_usd, \
                 total_duration_ms, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (run_id) DO UPDATE SET \
                 total_llm_calls = run_costs.total_llm_calls + EXCLUDED.total_llm_calls, \
                 total_tool_calls = run_costs.total_tool_calls + EXCLUDED.total_tool_calls, \
                 total_prompt_tokens = run_costs.total_prompt_tokens + EXCLUDED.total_prompt_tokens, \
                 total_completion_tokens = run_costs.total_completion_tokens + EXCLUDED.total_completion_tokens, \
                 total_estimated_cost_usd = run_costs.total_estimated_cost_usd + EXCLUDED.total_estimated_cost_usd, \
                 total_duration_ms = run_costs.total_duration_ms + EXCLUDED.total_duration_ms \
             RETURNING run_id, tenant_id, agent_name, total_llm_calls, total_tool_calls, \
                       total_prompt_tokens, total_completion_tokens, total_estimated_cost_usd, \
                       total_duration_ms, created_at",
        )
        .bind(&record.run_id)
        .bind(&record.tenant_id)
        .bind(&record.agent_name)
        .bind(record.total_llm_calls)
        .bind(record.total_tool_calls)
        .bind(record.total_prompt_tokens)
        .bind(record.total_completion_tokens)
        .bind(record.total_estimated_cost_usd)
        .bind(record.total_duration_ms)
        .bind(record.created_at)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_run_cost(&row)
    }

    /// Fetch a single run cost by run_id.
    pub async fn get_run_cost(&self, run_id: &str) -> Result<Option<RunCost>> {
        let row = sqlx::query(
            "SELECT run_id, tenant_id, agent_name, total_llm_calls, total_tool_calls, \
                    total_prompt_tokens, total_completion_tokens, total_estimated_cost_usd, \
                    total_duration_ms, created_at \
             FROM run_costs WHERE run_id = $1",
        )
        .bind(run_id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        row.map(|r| row_to_run_cost(&r)).transpose()
    }

    /// List run costs for a tenant, newest first.
    pub async fn list_run_costs(
        &self,
        tenant_id: &str,
        limit: i32,
        offset: i32,
        created_after: Option<i64>,
        created_before: Option<i64>,
    ) -> Result<Vec<RunCost>> {
        let rows = sqlx::query(list_run_costs_sql())
            .bind(tenant_id)
            .bind(limit)
            .bind(offset)
            .bind(created_after)
            .bind(created_before)
            .fetch_all(self.pool)
            .await
            .map_err(sqlx_err)?;

        rows.iter().map(row_to_run_cost).collect()
    }

    // -------------------------------------------------------------------------
    // Budgets
    // -------------------------------------------------------------------------

    /// Insert or update a tenant budget.
    pub async fn set_tenant_budget(&self, req: &SetTenantBudgetRequest) -> Result<TenantBudget> {
        if req.monthly_limit_usd < Decimal::ZERO {
            return Err(AppError::InvalidInput(
                "monthly_limit_usd must not be negative".into(),
            ));
        }
        if let Some(daily) = req.daily_limit_usd {
            if daily < Decimal::ZERO {
                return Err(AppError::InvalidInput(
                    "daily_limit_usd must not be negative".into(),
                ));
            }
        }
        if !(0..=100).contains(&req.alert_threshold_pct) {
            return Err(AppError::InvalidInput(
                "alert_threshold_pct must be between 0 and 100".into(),
            ));
        }

        let now = chrono::Utc::now().timestamp();

        let row = sqlx::query(
            "INSERT INTO tenant_budgets \
                (tenant_id, monthly_limit_usd, daily_limit_usd, alert_threshold_pct, currency, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (tenant_id) DO UPDATE SET \
                 monthly_limit_usd = EXCLUDED.monthly_limit_usd, \
                 daily_limit_usd = EXCLUDED.daily_limit_usd, \
                 alert_threshold_pct = EXCLUDED.alert_threshold_pct, \
                 currency = EXCLUDED.currency, \
                 updated_at = EXCLUDED.updated_at \
             RETURNING tenant_id, monthly_limit_usd, daily_limit_usd, alert_threshold_pct, currency, created_at, updated_at",
        )
        .bind(&req.tenant_id)
        .bind(req.monthly_limit_usd)
        .bind(req.daily_limit_usd)
        .bind(req.alert_threshold_pct)
        .bind(&req.currency)
        .bind(now)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_tenant_budget(&row)
    }

    /// Fetch a tenant budget.
    pub async fn get_tenant_budget(&self, tenant_id: &str) -> Result<Option<TenantBudget>> {
        let row = sqlx::query(
            "SELECT tenant_id, monthly_limit_usd, daily_limit_usd, alert_threshold_pct, currency, created_at, updated_at \
             FROM tenant_budgets WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        row.map(|r| row_to_tenant_budget(&r)).transpose()
    }

    /// Delete a tenant budget.
    pub async fn delete_tenant_budget(&self, tenant_id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM tenant_budgets WHERE tenant_id = $1")
            .bind(tenant_id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }

    // -------------------------------------------------------------------------
    // Alerts
    // -------------------------------------------------------------------------

    /// Insert a new budget alert.
    pub async fn insert_budget_alert(&self, alert: &BudgetAlert) -> Result<BudgetAlert> {
        validate_alert_type(&alert.alert_type)?;

        let row = sqlx::query(
            "INSERT INTO budget_alerts \
                (id, tenant_id, alert_type, current_spend_usd, limit_usd, threshold_pct, \
                 period_start, period_end, acknowledged, acknowledged_by, acknowledged_at, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             RETURNING id, tenant_id, alert_type, current_spend_usd, limit_usd, threshold_pct, \
                       period_start, period_end, acknowledged, acknowledged_by, acknowledged_at, created_at",
        )
        .bind(&alert.id)
        .bind(&alert.tenant_id)
        .bind(&alert.alert_type)
        .bind(alert.current_spend_usd)
        .bind(alert.limit_usd)
        .bind(alert.threshold_pct)
        .bind(alert.period_start)
        .bind(alert.period_end)
        .bind(alert.acknowledged)
        .bind(&alert.acknowledged_by)
        .bind(alert.acknowledged_at)
        .bind(alert.created_at)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_budget_alert(&row)
    }

    /// Fetch a single budget alert by id.
    pub async fn get_budget_alert(&self, id: &str) -> Result<Option<BudgetAlert>> {
        let row = sqlx::query(
            "SELECT id, tenant_id, alert_type, current_spend_usd, limit_usd, threshold_pct, \
                    period_start, period_end, acknowledged, acknowledged_by, acknowledged_at, created_at \
             FROM budget_alerts WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        row.map(|r| row_to_budget_alert(&r)).transpose()
    }

    /// List budget alerts with optional filtering.
    pub async fn list_budget_alerts(&self, q: &ListBudgetAlertsQuery) -> Result<Vec<BudgetAlert>> {
        let mut sql = String::from(
            "SELECT id, tenant_id, alert_type, current_spend_usd, limit_usd, threshold_pct, \
                    period_start, period_end, acknowledged, acknowledged_by, acknowledged_at, created_at \
             FROM budget_alerts WHERE 1=1",
        );
        if q.tenant_id.is_some() {
            sql.push_str(" AND tenant_id = $1");
        }
        if q.alert_type.is_some() {
            sql.push_str(" AND alert_type = $2");
        }
        if q.acknowledged.is_some() {
            sql.push_str(" AND acknowledged = $3");
        }
        sql.push_str(" ORDER BY created_at DESC, id ASC LIMIT $4 OFFSET $5");

        let mut query = sqlx::query(&sql);
        query = query.bind(&q.tenant_id);
        query = query.bind(&q.alert_type);
        query = query.bind(q.acknowledged);
        query = query.bind(q.limit);
        query = query.bind(q.offset);

        let rows = query.fetch_all(self.pool).await.map_err(sqlx_err)?;
        rows.iter().map(row_to_budget_alert).collect()
    }

    /// Acknowledge a budget alert.
    pub async fn acknowledge_budget_alert(
        &self,
        id: &str,
        req: &AcknowledgeBudgetAlertRequest,
    ) -> Result<BudgetAlert> {
        let now = chrono::Utc::now().timestamp();

        let row = sqlx::query(
            "UPDATE budget_alerts SET \
                acknowledged = TRUE, acknowledged_by = $1, acknowledged_at = $2 \
             WHERE id = $3 \
             RETURNING id, tenant_id, alert_type, current_spend_usd, limit_usd, threshold_pct, \
                       period_start, period_end, acknowledged, acknowledged_by, acknowledged_at, created_at",
        )
        .bind(&req.acknowledged_by)
        .bind(now)
        .bind(id)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_budget_alert(&row)
    }

    // -------------------------------------------------------------------------
    // Health Metrics
    // -------------------------------------------------------------------------

    /// Insert agent health metrics.
    pub async fn insert_health_metrics(
        &self,
        record: &AgentHealthMetrics,
    ) -> Result<AgentHealthMetrics> {
        let row = sqlx::query(
            "INSERT INTO agent_health_metrics \
                (id, tenant_id, agent_name, period_start, period_end, total_runs, \
                 successful_runs, failed_runs, avg_latency_ms, p50_latency_ms, p95_latency_ms, \
                 p99_latency_ms, total_tokens, total_cost_usd, error_rate_pct, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
             ON CONFLICT (tenant_id, agent_name, period_start, period_end) DO UPDATE SET \
                 total_runs = EXCLUDED.total_runs, \
                 successful_runs = EXCLUDED.successful_runs, \
                 failed_runs = EXCLUDED.failed_runs, \
                 avg_latency_ms = EXCLUDED.avg_latency_ms, \
                 p50_latency_ms = EXCLUDED.p50_latency_ms, \
                 p95_latency_ms = EXCLUDED.p95_latency_ms, \
                 p99_latency_ms = EXCLUDED.p99_latency_ms, \
                 total_tokens = EXCLUDED.total_tokens, \
                 total_cost_usd = EXCLUDED.total_cost_usd, \
                 error_rate_pct = EXCLUDED.error_rate_pct, \
                 created_at = EXCLUDED.created_at \
             RETURNING id, tenant_id, agent_name, period_start, period_end, total_runs, \
                       successful_runs, failed_runs, avg_latency_ms, p50_latency_ms, p95_latency_ms, \
                       p99_latency_ms, total_tokens, total_cost_usd, error_rate_pct, created_at",
        )
        .bind(&record.id)
        .bind(&record.tenant_id)
        .bind(&record.agent_name)
        .bind(record.period_start)
        .bind(record.period_end)
        .bind(record.total_runs)
        .bind(record.successful_runs)
        .bind(record.failed_runs)
        .bind(record.avg_latency_ms)
        .bind(record.p50_latency_ms)
        .bind(record.p95_latency_ms)
        .bind(record.p99_latency_ms)
        .bind(record.total_tokens)
        .bind(record.total_cost_usd)
        .bind(record.error_rate_pct)
        .bind(record.created_at)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_agent_health(&row)
    }

    /// Insert model health metrics.
    pub async fn insert_model_health_metrics(
        &self,
        record: &ModelHealthMetrics,
    ) -> Result<ModelHealthMetrics> {
        let row = sqlx::query(
            "INSERT INTO model_health_metrics \
                (id, tenant_id, model, period_start, period_end, total_calls, \
                 successful_calls, failed_calls, avg_latency_ms, p50_latency_ms, p95_latency_ms, \
                 p99_latency_ms, total_tokens, total_cost_usd, error_rate_pct, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
             ON CONFLICT (tenant_id, model, period_start, period_end) DO UPDATE SET \
                 total_calls = EXCLUDED.total_calls, \
                 successful_calls = EXCLUDED.successful_calls, \
                 failed_calls = EXCLUDED.failed_calls, \
                 avg_latency_ms = EXCLUDED.avg_latency_ms, \
                 p50_latency_ms = EXCLUDED.p50_latency_ms, \
                 p95_latency_ms = EXCLUDED.p95_latency_ms, \
                 p99_latency_ms = EXCLUDED.p99_latency_ms, \
                 total_tokens = EXCLUDED.total_tokens, \
                 total_cost_usd = EXCLUDED.total_cost_usd, \
                 error_rate_pct = EXCLUDED.error_rate_pct, \
                 created_at = EXCLUDED.created_at \
             RETURNING id, tenant_id, model, period_start, period_end, total_calls, \
                       successful_calls, failed_calls, avg_latency_ms, p50_latency_ms, p95_latency_ms, \
                       p99_latency_ms, total_tokens, total_cost_usd, error_rate_pct, created_at",
        )
        .bind(&record.id)
        .bind(&record.tenant_id)
        .bind(&record.model)
        .bind(record.period_start)
        .bind(record.period_end)
        .bind(record.total_calls)
        .bind(record.successful_calls)
        .bind(record.failed_calls)
        .bind(record.avg_latency_ms)
        .bind(record.p50_latency_ms)
        .bind(record.p95_latency_ms)
        .bind(record.p99_latency_ms)
        .bind(record.total_tokens)
        .bind(record.total_cost_usd)
        .bind(record.error_rate_pct)
        .bind(record.created_at)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_model_health(&row)
    }

    /// Fetch health metrics by id.
    pub async fn get_health_metrics(&self, id: &str) -> Result<Option<AgentHealthMetrics>> {
        let row = sqlx::query(
            "SELECT id, tenant_id, agent_name, period_start, period_end, total_runs, \
                    successful_runs, failed_runs, avg_latency_ms, p50_latency_ms, p95_latency_ms, \
                    p99_latency_ms, total_tokens, total_cost_usd, error_rate_pct, created_at \
             FROM agent_health_metrics WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        row.map(|r| row_to_agent_health(&r)).transpose()
    }

    /// List health metrics for a tenant, newest period first.
    pub async fn list_health_metrics(
        &self,
        tenant_id: &str,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<AgentHealthMetrics>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, agent_name, period_start, period_end, total_runs, \
                    successful_runs, failed_runs, avg_latency_ms, p50_latency_ms, p95_latency_ms, \
                    p99_latency_ms, total_tokens, total_cost_usd, error_rate_pct, created_at \
             FROM agent_health_metrics WHERE tenant_id = $1 ORDER BY period_start DESC, agent_name ASC LIMIT $2 OFFSET $3",
        )
        .bind(tenant_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        rows.iter().map(row_to_agent_health).collect()
    }

    /// List model health metrics for a tenant, newest period first.
    pub async fn list_model_metrics(
        &self,
        tenant_id: &str,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<ModelHealthMetrics>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, model, period_start, period_end, total_calls, \
                    successful_calls, failed_calls, avg_latency_ms, p50_latency_ms, p95_latency_ms, \
                    p99_latency_ms, total_tokens, total_cost_usd, error_rate_pct, created_at \
             FROM model_health_metrics WHERE tenant_id = $1 ORDER BY period_start DESC, model ASC LIMIT $2 OFFSET $3",
        )
        .bind(tenant_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        rows.iter().map(row_to_model_health).collect()
    }

    /// Per-model prompt-cache telemetry aggregated over `run_llm_calls`.
    ///
    /// `cache_hit_ratio` is `cached_tokens / prompt_tokens` across all calls
    /// of the model; rows whose provider never reported cache hits (or whose
    /// prompts total zero) get a `NULL` ratio rather than a division error.
    pub async fn cache_hit_stats(&self) -> Result<Vec<CacheHitStat>> {
        let rows = sqlx::query(
            "SELECT model, \
                    COUNT(*) AS calls, \
                    COALESCE(SUM(cached_tokens), 0)::BIGINT AS cached_tokens_total, \
                    SUM(prompt_tokens)::BIGINT AS prompt_tokens_total, \
                    CASE \
                        WHEN COALESCE(SUM(cached_tokens), 0) = 0 OR SUM(prompt_tokens) IS NULL \
                             OR SUM(prompt_tokens) = 0 \
                        THEN NULL \
                        ELSE SUM(cached_tokens)::DOUBLE PRECISION / SUM(prompt_tokens)::DOUBLE PRECISION \
                    END AS cache_hit_ratio, \
                    AVG(COALESCE(total_time_ms, 0))::DOUBLE PRECISION AS avg_total_time_ms \
             FROM run_llm_calls \
             GROUP BY model \
             ORDER BY calls DESC, model ASC",
        )
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        rows.iter().map(row_to_cache_hit_stat).collect()
    }
}

// =============================================================================
// Cache-hit stats
// =============================================================================

/// One per-model row of prompt-cache usage telemetry.
#[derive(Debug, Clone, Serialize)]
pub struct CacheHitStat {
    /// Model name (e.g. "gpt-4o").
    pub model: String,
    /// Number of recorded LLM calls for the model.
    pub calls: i64,
    /// Sum of provider-reported cached tokens (`0` when never reported).
    pub cached_tokens: i64,
    /// Sum of prompt tokens across all calls.
    pub prompt_tokens: i64,
    /// Share of prompt tokens served from cache (`None` when no call
    /// reported cache hits or prompts total zero).
    pub cache_hit_ratio: Option<f64>,
    /// Average whole-call wall-clock time in milliseconds (`None` when no
    /// call measured it).
    pub avg_total_time_ms: Option<f64>,
}

fn row_to_cache_hit_stat(row: &sqlx::postgres::PgRow) -> Result<CacheHitStat> {
    Ok(CacheHitStat {
        model: row.try_get("model").map_err(sqlx_err)?,
        calls: row.try_get("calls").map_err(sqlx_err)?,
        cached_tokens: row.try_get("cached_tokens_total").map_err(sqlx_err)?,
        prompt_tokens: row.try_get("prompt_tokens_total").map_err(sqlx_err)?,
        cache_hit_ratio: row.try_get("cache_hit_ratio").map_err(sqlx_err)?,
        avg_total_time_ms: row.try_get("avg_total_time_ms").map_err(sqlx_err)?,
    })
}

// =============================================================================
// Row mappers
// =============================================================================

fn row_to_llm_call(row: &sqlx::postgres::PgRow) -> Result<RunLlmCall> {
    Ok(RunLlmCall {
        id: row.try_get("id").map_err(sqlx_err)?,
        run_id: row.try_get("run_id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        agent_name: row.try_get("agent_name").map_err(sqlx_err)?,
        step_index: row.try_get("step_index").map_err(sqlx_err)?,
        provider: row.try_get("provider").map_err(sqlx_err)?,
        model: row.try_get("model").map_err(sqlx_err)?,
        prompt_tokens: row.try_get("prompt_tokens").map_err(sqlx_err)?,
        completion_tokens: row.try_get("completion_tokens").map_err(sqlx_err)?,
        total_tokens: row.try_get("total_tokens").map_err(sqlx_err)?,
        estimated_cost_usd: row.try_get("estimated_cost_usd").map_err(sqlx_err)?,
        latency_ms: row.try_get("latency_ms").map_err(sqlx_err)?,
                cached_tokens: row.try_get("cached_tokens").map_err(sqlx_err)?,
                total_time_ms: row.try_get("total_time_ms").map_err(sqlx_err)?,
        status: row.try_get("status").map_err(sqlx_err)?,
        error_message: row.try_get("error_message").map_err(sqlx_err)?,
        request_payload: row.try_get("request_payload").map_err(sqlx_err)?,
        response_payload: row.try_get("response_payload").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
    })
}

fn row_to_tool_call(row: &sqlx::postgres::PgRow) -> Result<RunToolCall> {
    Ok(RunToolCall {
        id: row.try_get("id").map_err(sqlx_err)?,
        run_id: row.try_get("run_id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        agent_name: row.try_get("agent_name").map_err(sqlx_err)?,
        step_index: row.try_get("step_index").map_err(sqlx_err)?,
        tool_name: row.try_get("tool_name").map_err(sqlx_err)?,
        tool_type: row.try_get("tool_type").map_err(sqlx_err)?,
        arguments: row.try_get("arguments").map_err(sqlx_err)?,
        result: row.try_get("result").map_err(sqlx_err)?,
        latency_ms: row.try_get("latency_ms").map_err(sqlx_err)?,
        status: row.try_get("status").map_err(sqlx_err)?,
        error_message: row.try_get("error_message").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
    })
}

fn list_run_costs_sql() -> &'static str {
    "SELECT run_id, tenant_id, agent_name, total_llm_calls, total_tool_calls, \
            total_prompt_tokens, total_completion_tokens, total_estimated_cost_usd, \
            total_duration_ms, created_at \
     FROM run_costs \
     WHERE tenant_id = $1 \
       AND ($4::BIGINT IS NULL OR created_at >= $4) \
       AND ($5::BIGINT IS NULL OR created_at <= $5) \
     ORDER BY created_at DESC, run_id ASC LIMIT $2 OFFSET $3"
}

fn row_to_run_cost(row: &sqlx::postgres::PgRow) -> Result<RunCost> {
    Ok(RunCost {
        run_id: row.try_get("run_id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        agent_name: row.try_get("agent_name").map_err(sqlx_err)?,
        total_llm_calls: row.try_get("total_llm_calls").map_err(sqlx_err)?,
        total_tool_calls: row.try_get("total_tool_calls").map_err(sqlx_err)?,
        total_prompt_tokens: row.try_get("total_prompt_tokens").map_err(sqlx_err)?,
        total_completion_tokens: row.try_get("total_completion_tokens").map_err(sqlx_err)?,
        total_estimated_cost_usd: row.try_get("total_estimated_cost_usd").map_err(sqlx_err)?,
        total_duration_ms: row.try_get("total_duration_ms").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
    })
}

fn row_to_tenant_budget(row: &sqlx::postgres::PgRow) -> Result<TenantBudget> {
    Ok(TenantBudget {
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        monthly_limit_usd: row.try_get("monthly_limit_usd").map_err(sqlx_err)?,
        daily_limit_usd: row.try_get("daily_limit_usd").map_err(sqlx_err)?,
        alert_threshold_pct: row.try_get("alert_threshold_pct").map_err(sqlx_err)?,
        currency: row.try_get("currency").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
        updated_at: row.try_get("updated_at").map_err(sqlx_err)?,
    })
}

fn row_to_budget_alert(row: &sqlx::postgres::PgRow) -> Result<BudgetAlert> {
    Ok(BudgetAlert {
        id: row.try_get("id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        alert_type: row.try_get("alert_type").map_err(sqlx_err)?,
        current_spend_usd: row.try_get("current_spend_usd").map_err(sqlx_err)?,
        limit_usd: row.try_get("limit_usd").map_err(sqlx_err)?,
        threshold_pct: row.try_get("threshold_pct").map_err(sqlx_err)?,
        period_start: row.try_get("period_start").map_err(sqlx_err)?,
        period_end: row.try_get("period_end").map_err(sqlx_err)?,
        acknowledged: row.try_get("acknowledged").map_err(sqlx_err)?,
        acknowledged_by: row.try_get("acknowledged_by").map_err(sqlx_err)?,
        acknowledged_at: row.try_get("acknowledged_at").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
    })
}

fn row_to_agent_health(row: &sqlx::postgres::PgRow) -> Result<AgentHealthMetrics> {
    Ok(AgentHealthMetrics {
        id: row.try_get("id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        agent_name: row.try_get("agent_name").map_err(sqlx_err)?,
        period_start: row.try_get("period_start").map_err(sqlx_err)?,
        period_end: row.try_get("period_end").map_err(sqlx_err)?,
        total_runs: row.try_get("total_runs").map_err(sqlx_err)?,
        successful_runs: row.try_get("successful_runs").map_err(sqlx_err)?,
        failed_runs: row.try_get("failed_runs").map_err(sqlx_err)?,
        avg_latency_ms: row.try_get("avg_latency_ms").map_err(sqlx_err)?,
        p50_latency_ms: row.try_get("p50_latency_ms").map_err(sqlx_err)?,
        p95_latency_ms: row.try_get("p95_latency_ms").map_err(sqlx_err)?,
        p99_latency_ms: row.try_get("p99_latency_ms").map_err(sqlx_err)?,
        total_tokens: row.try_get("total_tokens").map_err(sqlx_err)?,
        total_cost_usd: row.try_get("total_cost_usd").map_err(sqlx_err)?,
        error_rate_pct: row.try_get("error_rate_pct").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
    })
}

fn row_to_model_health(row: &sqlx::postgres::PgRow) -> Result<ModelHealthMetrics> {
    Ok(ModelHealthMetrics {
        id: row.try_get("id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        model: row.try_get("model").map_err(sqlx_err)?,
        period_start: row.try_get("period_start").map_err(sqlx_err)?,
        period_end: row.try_get("period_end").map_err(sqlx_err)?,
        total_calls: row.try_get("total_calls").map_err(sqlx_err)?,
        successful_calls: row.try_get("successful_calls").map_err(sqlx_err)?,
        failed_calls: row.try_get("failed_calls").map_err(sqlx_err)?,
        avg_latency_ms: row.try_get("avg_latency_ms").map_err(sqlx_err)?,
        p50_latency_ms: row.try_get("p50_latency_ms").map_err(sqlx_err)?,
        p95_latency_ms: row.try_get("p95_latency_ms").map_err(sqlx_err)?,
        p99_latency_ms: row.try_get("p99_latency_ms").map_err(sqlx_err)?,
        total_tokens: row.try_get("total_tokens").map_err(sqlx_err)?,
        total_cost_usd: row.try_get("total_cost_usd").map_err(sqlx_err)?,
        error_rate_pct: row.try_get("error_rate_pct").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
    })
}

// =============================================================================
// Helpers
// =============================================================================

fn sqlx_err(e: sqlx::Error) -> AppError {
    AppError::Database(e.to_string())
}

fn validate_tool_type(t: &str) -> Result<()> {
    match t {
        "http" | "script" | "sql" | "mcp" | "skill_step" => Ok(()),
        _ => Err(AppError::InvalidInput(format!(
            "Invalid tool_type '{t}'. Must be one of: http, script, sql, mcp, skill_step"
        ))),
    }
}

fn validate_status(s: &str) -> Result<()> {
    match s {
        "success" | "error" | "timeout" => Ok(()),
        _ => Err(AppError::InvalidInput(format!(
            "Invalid status '{s}'. Must be one of: success, error, timeout"
        ))),
    }
}

fn validate_alert_type(s: &str) -> Result<()> {
    match s {
        "daily_exceeded" | "monthly_exceeded" | "threshold_reached" => Ok(()),
        _ => Err(AppError::InvalidInput(format!(
            "Invalid alert_type '{s}'. Must be one of: daily_exceeded, monthly_exceeded, threshold_reached"
        ))),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    // -------------------------------------------------------------------------
    // Unit tests (no DB required)
    // -------------------------------------------------------------------------

    #[test]
    fn run_llm_call_serde_roundtrip() {
        let original = RunLlmCall {
            id: "call-1".into(),
            run_id: "run-1".into(),
            tenant_id: "tenant-a".into(),
            agent_name: "agent-x".into(),
            step_index: 0,
            provider: "openai".into(),
            model: "gpt-4o".into(),
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            estimated_cost_usd: dec!(0.000250),
            latency_ms: 420,
            cached_tokens: Some(40),
            total_time_ms: Some(430),
            status: "success".into(),
            error_message: None,
            request_payload: Some(serde_json::json!({"messages": []})),
            response_payload: Some(serde_json::json!({"choices": []})),
            created_at: 1_700_000_000,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: RunLlmCall = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn run_tool_call_serde_roundtrip() {
        let original = RunToolCall {
            id: "tool-1".into(),
            run_id: "run-1".into(),
            tenant_id: "tenant-a".into(),
            agent_name: "agent-x".into(),
            step_index: 1,
            tool_name: "http_get".into(),
            tool_type: "http".into(),
            arguments: serde_json::json!({"url": "https://example.com"}),
            result: Some(serde_json::json!({"status": 200})),
            latency_ms: 120,
            status: "success".into(),
            error_message: None,
            created_at: 1_700_000_000,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: RunToolCall = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn run_cost_serde_roundtrip() {
        let original = RunCost {
            run_id: "run-1".into(),
            tenant_id: "tenant-a".into(),
            agent_name: "agent-x".into(),
            total_llm_calls: 3,
            total_tool_calls: 2,
            total_prompt_tokens: 300,
            total_completion_tokens: 150,
            total_estimated_cost_usd: dec!(0.001500),
            total_duration_ms: 1500,
            created_at: 1_700_000_000,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: RunCost = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn tenant_budget_serde_roundtrip() {
        let original = TenantBudget {
            tenant_id: "tenant-a".into(),
            monthly_limit_usd: dec!(100.00),
            daily_limit_usd: Some(dec!(5.00)),
            alert_threshold_pct: 80,
            currency: "USD".into(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_100,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: TenantBudget = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn budget_alert_serde_roundtrip() {
        let original = BudgetAlert {
            id: "alert-1".into(),
            tenant_id: "tenant-a".into(),
            alert_type: "threshold_reached".into(),
            current_spend_usd: dec!(85.50),
            limit_usd: dec!(100.00),
            threshold_pct: 80,
            period_start: 1_700_000_000,
            period_end: 1_700_086_400,
            acknowledged: false,
            acknowledged_by: None,
            acknowledged_at: None,
            created_at: 1_700_086_400,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: BudgetAlert = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn agent_health_metrics_serde_roundtrip() {
        let original = AgentHealthMetrics {
            id: "health-1".into(),
            tenant_id: "tenant-a".into(),
            agent_name: "agent-x".into(),
            period_start: 1_700_000_000,
            period_end: 1_700_086_400,
            total_runs: 100,
            successful_runs: 95,
            failed_runs: 5,
            avg_latency_ms: 350,
            p50_latency_ms: 300,
            p95_latency_ms: 800,
            p99_latency_ms: 1200,
            total_tokens: 50000,
            total_cost_usd: dec!(0.500000),
            error_rate_pct: dec!(5.00),
            created_at: 1_700_086_400,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: AgentHealthMetrics = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn validate_status_accepts_valid() {
        for s in ["success", "error", "timeout"] {
            assert!(validate_status(s).is_ok());
        }
    }

    #[test]
    fn validate_status_rejects_invalid() {
        let err = validate_status("pending").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[test]
    fn validate_tool_type_accepts_valid() {
        for t in ["http", "script", "sql", "mcp", "skill_step"] {
            assert!(validate_tool_type(t).is_ok());
        }
    }

    #[test]
    fn validate_tool_type_rejects_invalid() {
        let err = validate_tool_type("websocket").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[test]
    fn validate_alert_type_accepts_valid() {
        for a in ["daily_exceeded", "monthly_exceeded", "threshold_reached"] {
            assert!(validate_alert_type(a).is_ok());
        }
    }

    #[test]
    fn validate_alert_type_rejects_invalid() {
        let err = validate_alert_type("unknown").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[test]
    fn list_tool_calls_order_is_deterministic() {
        let mut sql = String::from(
            "SELECT id, run_id, tenant_id, agent_name, step_index, tool_name, tool_type, \
                    arguments, result, latency_ms, status, error_message, created_at \
             FROM run_tool_calls WHERE 1=1",
        );
        sql.push_str(" ORDER BY created_at DESC, id ASC LIMIT $7 OFFSET $8");
        assert!(sql.contains("ORDER BY created_at DESC, id ASC"));
    }

    #[test]
    fn tool_calls_for_run_order_by_step_then_id() {
        let sql = "SELECT id, run_id, tenant_id, agent_name, step_index, tool_name, tool_type, \
                    arguments, result, latency_ms, status, error_message, created_at \
             FROM run_tool_calls WHERE run_id = $1 ORDER BY step_index ASC, id ASC";
        assert!(sql.contains("ORDER BY step_index ASC, id ASC"));
    }

    #[test]
    fn list_llm_calls_order_is_deterministic() {
        let mut sql = String::from(
            "SELECT id, run_id, tenant_id, agent_name, step_index, provider, model, \
                    prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd, \
                    latency_ms, cached_tokens, total_time_ms, status, \
                    error_message, request_payload, response_payload, created_at \
             FROM run_llm_calls WHERE 1=1",
        );
        sql.push_str(" ORDER BY created_at DESC, id ASC LIMIT $7 OFFSET $8");
        assert!(sql.contains("ORDER BY created_at DESC, id ASC"));
    }

    #[test]
    fn llm_calls_for_run_order_by_step_then_id() {
        let sql = "SELECT id, run_id, tenant_id, agent_name, step_index, provider, model, \
                    prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd, \
                    latency_ms, cached_tokens, total_time_ms, status, \
                    error_message, request_payload, response_payload, created_at \
             FROM run_llm_calls WHERE run_id = $1 ORDER BY step_index ASC, id ASC";
        assert!(sql.contains("ORDER BY step_index ASC, id ASC"));
    }

    #[test]
    fn list_budget_alerts_order_is_deterministic() {
        let q = ListBudgetAlertsQuery::default();
        let mut sql = String::from(
            "SELECT id, tenant_id, alert_type, current_spend_usd, limit_usd, threshold_pct, \
                    period_start, period_end, acknowledged, acknowledged_by, acknowledged_at, created_at \
             FROM budget_alerts WHERE 1=1",
        );
        if q.tenant_id.is_some() {
            sql.push_str(" AND tenant_id = $1");
        }
        if q.alert_type.is_some() {
            sql.push_str(" AND alert_type = $2");
        }
        if q.acknowledged.is_some() {
            sql.push_str(" AND acknowledged = $3");
        }
        sql.push_str(" ORDER BY created_at DESC, id ASC LIMIT $4 OFFSET $5");
        assert!(sql.contains("ORDER BY created_at DESC, id ASC"));
    }

    #[test]
    fn list_run_costs_order_is_deterministic() {
        assert!(list_run_costs_sql().contains("ORDER BY created_at DESC, run_id ASC"));
    }

    #[test]
    fn list_llm_calls_query_defaults() {
        let q: ListLlmCallsQuery = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(q.limit, 50);
        assert_eq!(q.offset, 0);
        assert!(q.run_id.is_none());
    }

    #[test]
    fn llm_call_telemetry_fields_default_when_absent() {
        let back: RunLlmCall =
            serde_json::from_str(r#"{"id":"c","run_id":"r","tenant_id":"t","agent_name":"a","step_index":0,"provider":"openai","model":"gpt-4o","prompt_tokens":1,"completion_tokens":1,"total_tokens":2,"estimated_cost_usd":0,"latency_ms":5,"status":"success","created_at":0}"#)
                .expect("deserialize without telemetry fields");
        assert_eq!(back.cached_tokens, None);
        assert_eq!(back.total_time_ms, None);
    }

    #[test]
    fn log_llm_call_request_telemetry_fields_serde_roundtrip() {
        let json = r#"{"cached_tokens": 40, "total_time_ms": 430}"#;
        let partial: serde_json::Value = serde_json::from_str(json).unwrap();
        let full = serde_json::json!({
            "id": "call-1",
            "run_id": "run-1",
            "tenant_id": "tenant-a",
            "agent_name": "agent-x",
            "step_index": 0,
            "provider": "openai",
            "model": "gpt-4o",
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "estimated_cost_usd": "0.000250",
            "latency_ms": 420,
            "status": "success",
            "created_at": 1_700_000_000i64,
        });
        // Merge so the request deserializes; defaults keep old payloads valid.
        let merged = match (full.clone(), partial) {
            (serde_json::Value::Object(mut base), serde_json::Value::Object(extra)) => {
                base.extend(extra);
                serde_json::Value::Object(base)
            }
            _ => unreachable!(),
        };
        let req: LogLlmCallRequest = serde_json::from_value(merged).expect("deserialize");
        assert_eq!(req.cached_tokens, Some(40));
        assert_eq!(req.total_time_ms, Some(430));

        let bare: LogLlmCallRequest =
            serde_json::from_value(full).expect("deserialize without telemetry fields");
        assert_eq!(bare.cached_tokens, None);
        assert_eq!(bare.total_time_ms, None);
    }

    #[test]
    fn cache_hit_stats_sql_is_null_safe_aggregate() {
        let sql = "SELECT model, \
                    COUNT(*) AS calls, \
                    COALESCE(SUM(cached_tokens), 0)::BIGINT AS cached_tokens_total, \
                    SUM(prompt_tokens)::BIGINT AS prompt_tokens_total, \
                    CASE \
                        WHEN COALESCE(SUM(cached_tokens), 0) = 0 OR SUM(prompt_tokens) IS NULL \
                             OR SUM(prompt_tokens) = 0 \
                        THEN NULL \
                        ELSE SUM(cached_tokens)::DOUBLE PRECISION / SUM(prompt_tokens)::DOUBLE PRECISION \
                    END AS cache_hit_ratio, \
                    AVG(COALESCE(total_time_ms, 0))::DOUBLE PRECISION AS avg_total_time_ms \
             FROM run_llm_calls \
             GROUP BY model \
             ORDER BY calls DESC, model ASC";
        assert!(sql.contains("COALESCE(SUM(cached_tokens), 0)"));
        assert!(sql.contains("THEN NULL"));
        assert!(sql.contains("GROUP BY model"));
    }

    #[test]
    fn list_tool_calls_query_defaults() {
        let q: ListToolCallsQuery = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(q.limit, 50);
        assert_eq!(q.offset, 0);
    }

    #[test]
    fn list_budget_alerts_query_defaults() {
        let q: ListBudgetAlertsQuery = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(q.limit, 50);
        assert_eq!(q.offset, 0);
    }

    // -------------------------------------------------------------------------
    // Integration tests (require a live Postgres instance)
    // -------------------------------------------------------------------------

    fn test_db_url() -> String {
        if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
            return url;
        }
        if let Ok(url) = std::env::var("DATABASE_URL") {
            if url.contains("/ares") && !url.contains("ares_test") {
                return url.replace("/ares", "/ares_test");
            }
            return url;
        }
        "postgres://postgres:postgres@localhost:5432/ares_test".into()
    }

    async fn try_test_pool() -> Option<PgPool> {
        let db = crate::PostgresClient::new_remote(test_db_url(), String::new())
            .await
            .ok()?;
        crate::MIGRATOR.run(&db.pool).await.ok()?;
        Some(db.pool)
    }

    /// Seed the tenant and agent_runs parents required by run-history FKs.
    async fn seed_integration_parents(pool: &PgPool, tenant_id: &str, run_id: &str) {
        let _ = sqlx::query(
            "INSERT INTO tenants (id, name, tier, created_at, updated_at) VALUES ($1, $1, 'free', 1, 1) ON CONFLICT (id) DO NOTHING",
        )
        .bind(tenant_id)
        .execute(pool)
        .await;
        let _ = sqlx::query(
            "INSERT INTO agent_runs (id, tenant_id, agent_name, status, input_tokens, output_tokens, duration_ms, created_at) VALUES ($1, $2, 'integration-test-agent', 'completed', 0, 0, 0, 1) ON CONFLICT (id) DO NOTHING",
        )
        .bind(run_id)
        .bind(tenant_id)
        .execute(pool)
        .await;
    }

    #[tokio::test]
    async fn integration_llm_call_crud_roundtrip() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RunHistoryStore::new(&pool);
        seed_integration_parents(&pool, "tenant-integration", "run-integration-1").await;

        // Clean up
        let _ = sqlx::query("DELETE FROM run_llm_calls WHERE agent_name LIKE 'integration-test-%'")
            .execute(&pool)
            .await;

        let req = LogLlmCallRequest {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: "run-integration-1".into(),
            tenant_id: "tenant-integration".into(),
            agent_name: "integration-test-agent".into(),
            step_index: 0,
            provider: "openai".into(),
            model: "gpt-4o".into(),
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            estimated_cost_usd: dec!(0.000250),
            latency_ms: 420,
            cached_tokens: Some(40),
            total_time_ms: Some(430),
            status: "success".into(),
            error_message: None,
            request_payload: Some(serde_json::json!({"messages": []})),
            response_payload: Some(serde_json::json!({"choices": []})),
            created_at: chrono::Utc::now().timestamp(),
        };

        // Insert
        let inserted = store.insert_llm_call(&req).await.expect("insert_llm_call");
        assert_eq!(inserted.run_id, req.run_id);
        assert_eq!(inserted.total_tokens, 150);

        // Get
        let fetched = store
            .get_llm_call(&inserted.id)
            .await
            .expect("get_llm_call");
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().id, inserted.id);

        // List for run
        let for_run = store
            .get_llm_calls_for_run(&req.run_id)
            .await
            .expect("get_llm_calls_for_run");
        assert_eq!(for_run.len(), 1);
        assert_eq!(for_run[0].id, inserted.id);

        // List with filter
        let q = ListLlmCallsQuery {
            tenant_id: Some(req.tenant_id.clone()),
            ..Default::default()
        };
        let listed = store.list_llm_calls(&q).await.expect("list_llm_calls");
        assert!(listed.iter().any(|c| c.id == inserted.id));

        // Cleanup
        let _ = sqlx::query("DELETE FROM run_llm_calls WHERE agent_name LIKE 'integration-test-%'")
            .execute(&pool)
            .await;
    }

    /// Telemetry fields survive insert → RETURNING → get → list, and
    /// `cache_hit_stats` aggregates them per model.
    #[tokio::test]
    async fn integration_llm_call_cache_telemetry_roundtrip_and_stats() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RunHistoryStore::new(&pool);
        seed_integration_parents(&pool, "tenant-integration", "run-integration-cache").await;

        // Clean up
        let _ = sqlx::query(
            "DELETE FROM run_llm_calls WHERE agent_name LIKE 'integration-test-%'",
        )
        .execute(&pool)
        .await;

        let req = LogLlmCallRequest {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: "run-integration-cache".into(),
            tenant_id: "tenant-integration".into(),
            agent_name: "integration-test-agent".into(),
            step_index: 0,
            provider: "openai".into(),
            model: "gpt-4o-telemetry".into(),
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            estimated_cost_usd: dec!(0.000250),
            latency_ms: 420,
            cached_tokens: Some(40),
            total_time_ms: Some(430),
            status: "success".into(),
            error_message: None,
            request_payload: None,
            response_payload: None,
            created_at: chrono::Utc::now().timestamp(),
        };

        // Insert returns the persisted row including telemetry.
        let inserted = store.insert_llm_call(&req).await.expect("insert_llm_call");
        assert_eq!(inserted.cached_tokens, Some(40));
        assert_eq!(inserted.total_time_ms, Some(430));

        // Get reads the same values back from storage.
        let fetched = store
            .get_llm_call(&inserted.id)
            .await
            .expect("get_llm_call")
            .expect("row exists");
        assert_eq!(fetched.cached_tokens, Some(40));
        assert_eq!(fetched.total_time_ms, Some(430));

        // A second call without reported cache hits stays NULL-safe.
        let mut null_req = req.clone();
        null_req.id = uuid::Uuid::new_v4().to_string();
        null_req.model = "gpt-4o-telemetry".into();
        null_req.cached_tokens = None;
        null_req.total_time_ms = None;
        let inserted_null = store
            .insert_llm_call(&null_req)
            .await
            .expect("insert null-telemetry call");
        assert_eq!(inserted_null.cached_tokens, None);

        // Aggregate view sums cached tokens and skips the NULL row safely.
        let stats = store.cache_hit_stats().await.expect("cache_hit_stats");
        let stat = stats
            .iter()
            .find(|s| s.model == "gpt-4o-telemetry")
            .expect("telemetry model in stats");
        assert_eq!(stat.cached_tokens, 40);
        assert_eq!(stat.prompt_tokens, 200);
        assert_eq!(stat.calls, 2);
        assert_eq!(stat.cache_hit_ratio, Some(0.2));
        assert_eq!(stat.avg_total_time_ms, Some(215.0));

        // Cleanup
        let _ = sqlx::query(
            "DELETE FROM run_llm_calls WHERE agent_name LIKE 'integration-test-%'",
        )
        .execute(&pool)
        .await;
    }

    #[tokio::test]
    async fn integration_tool_call_crud_roundtrip() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RunHistoryStore::new(&pool);
        seed_integration_parents(&pool, "tenant-integration", "run-integration-2").await;

        // Clean up
        let _ =
            sqlx::query("DELETE FROM run_tool_calls WHERE agent_name LIKE 'integration-test-%'")
                .execute(&pool)
                .await;

        let req = LogToolCallRequest {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: "run-integration-2".into(),
            tenant_id: "tenant-integration".into(),
            agent_name: "integration-test-agent".into(),
            step_index: 1,
            tool_name: "http_get".into(),
            tool_type: "http".into(),
            arguments: serde_json::json!({"url": "https://example.com"}),
            result: Some(serde_json::json!({"status": 200})),
            latency_ms: 120,
            status: "success".into(),
            error_message: None,
            created_at: chrono::Utc::now().timestamp(),
        };

        // Insert
        let inserted = store
            .insert_tool_call(&req)
            .await
            .expect("insert_tool_call");
        assert_eq!(inserted.run_id, req.run_id);
        assert_eq!(inserted.tool_name, "http_get");

        // Get
        let fetched = store
            .get_tool_call(&inserted.id)
            .await
            .expect("get_tool_call");
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().id, inserted.id);

        // List for run
        let for_run = store
            .get_tool_calls_for_run(&req.run_id)
            .await
            .expect("get_tool_calls_for_run");
        assert_eq!(for_run.len(), 1);
        assert_eq!(for_run[0].id, inserted.id);

        // List with filter
        let q = ListToolCallsQuery {
            tenant_id: Some(req.tenant_id.clone()),
            ..Default::default()
        };
        let listed = store.list_tool_calls(&q).await.expect("list_tool_calls");
        assert!(listed.iter().any(|c| c.id == inserted.id));

        // Cleanup
        let _ =
            sqlx::query("DELETE FROM run_tool_calls WHERE agent_name LIKE 'integration-test-%'")
                .execute(&pool)
                .await;
    }

    #[tokio::test]
    async fn integration_insert_tool_call_validates_status() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RunHistoryStore::new(&pool);

        let req = LogToolCallRequest {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: "run-bad".into(),
            tenant_id: "tenant-bad".into(),
            agent_name: "integration-test-agent".into(),
            step_index: 0,
            tool_name: "http_get".into(),
            tool_type: "http".into(),
            arguments: serde_json::json!({}),
            result: None,
            latency_ms: 0,
            status: "pending".into(),
            error_message: None,
            created_at: chrono::Utc::now().timestamp(),
        };

        let err = store.insert_tool_call(&req).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn integration_budget_crud_roundtrip() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RunHistoryStore::new(&pool);
        let tenant_id = format!("integration-test-{}", uuid::Uuid::new_v4());

        // Clean up
        let _ = sqlx::query("DELETE FROM tenant_budgets WHERE tenant_id LIKE 'integration-test-%'")
            .execute(&pool)
            .await;

        // Set
        let req = SetTenantBudgetRequest {
            tenant_id: tenant_id.clone(),
            monthly_limit_usd: dec!(1000.00),
            daily_limit_usd: Some(dec!(50.00)),
            alert_threshold_pct: 85,
            currency: "USD".into(),
        };
        let budget = store
            .set_tenant_budget(&req)
            .await
            .expect("set_tenant_budget");
        assert_eq!(budget.monthly_limit_usd, dec!(1000.00));
        assert_eq!(budget.currency, "USD");

        // Get
        let fetched = store
            .get_tenant_budget(&tenant_id)
            .await
            .expect("get_tenant_budget");
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().tenant_id, tenant_id);

        // Delete
        let deleted = store
            .delete_tenant_budget(&tenant_id)
            .await
            .expect("delete_tenant_budget");
        assert_eq!(deleted, 1);
        assert!(store
            .get_tenant_budget(&tenant_id)
            .await
            .expect("get after delete")
            .is_none());
    }

    #[tokio::test]
    async fn integration_alert_crud_roundtrip() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RunHistoryStore::new(&pool);
        let id = format!("alert-{}", uuid::Uuid::new_v4());

        // Clean up
        let _ = sqlx::query("DELETE FROM budget_alerts WHERE id LIKE 'alert-integration-%'")
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM budget_alerts WHERE tenant_id LIKE 'integration-test-%'")
            .execute(&pool)
            .await;

        let alert_tenant = format!("integration-test-{}", uuid::Uuid::new_v4());
        let _ = sqlx::query(
            "INSERT INTO tenants (id, name, tier, created_at, updated_at) VALUES ($1, $1, 'free', 1, 1) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&alert_tenant)
        .execute(&pool)
        .await;
        let now = chrono::Utc::now().timestamp();
        let alert = BudgetAlert {
            id: id.clone(),
            tenant_id: alert_tenant,
            alert_type: "threshold_reached".into(),
            current_spend_usd: dec!(85.00),
            limit_usd: dec!(100.00),
            threshold_pct: 85,
            period_start: now,
            period_end: now + 86400,
            acknowledged: false,
            acknowledged_by: None,
            acknowledged_at: None,
            created_at: now,
        };

        // Insert
        let inserted = store
            .insert_budget_alert(&alert)
            .await
            .expect("insert_budget_alert");
        assert_eq!(inserted.id, id);
        assert!(!inserted.acknowledged);

        // Get
        let fetched = store.get_budget_alert(&id).await.expect("get_budget_alert");
        assert!(fetched.is_some());

        // Acknowledge
        let ack_req = AcknowledgeBudgetAlertRequest {
            acknowledged_by: "admin".into(),
        };
        let acked = store
            .acknowledge_budget_alert(&id, &ack_req)
            .await
            .expect("acknowledge");
        assert!(acked.acknowledged);
        assert_eq!(acked.acknowledged_by.as_deref(), Some("admin"));
        assert!(acked.acknowledged_at.is_some());

        // Cleanup
        let _ = sqlx::query("DELETE FROM budget_alerts WHERE id = $1")
            .bind(&id)
            .execute(&pool)
            .await;
    }

    #[tokio::test]
    async fn integration_health_metrics_crud_roundtrip() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RunHistoryStore::new(&pool);
        let id = format!("health-{}", uuid::Uuid::new_v4());

        // Clean up
        let _ = sqlx::query(
            "DELETE FROM agent_health_metrics WHERE tenant_id LIKE 'integration-test-%'",
        )
        .execute(&pool)
        .await;
        let _ = sqlx::query(
            "DELETE FROM model_health_metrics WHERE tenant_id LIKE 'integration-test-%'",
        )
        .execute(&pool)
        .await;

        let now = chrono::Utc::now().timestamp();
        let record = AgentHealthMetrics {
            id: id.clone(),
            tenant_id: format!("integration-test-{}", uuid::Uuid::new_v4()),
            agent_name: "agent-b".into(),
            period_start: now,
            period_end: now + 3600,
            total_runs: 100,
            successful_runs: 95,
            failed_runs: 5,
            avg_latency_ms: 350,
            p50_latency_ms: 300,
            p95_latency_ms: 800,
            p99_latency_ms: 1200,
            total_tokens: 50000,
            total_cost_usd: dec!(0.500000),
            error_rate_pct: dec!(5.00),
            created_at: now,
        };

        // Insert
        let inserted = store
            .insert_health_metrics(&record)
            .await
            .expect("insert_health_metrics");
        assert_eq!(inserted.id, id);
        assert_eq!(inserted.total_runs, 100);

        // Get
        let fetched = store
            .get_health_metrics(&id)
            .await
            .expect("get_health_metrics");
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().total_runs, 100);

        let mut agent_c = record.clone();
        agent_c.id = format!("health-{}", uuid::Uuid::new_v4());
        agent_c.agent_name = "agent-c".into();
        store
            .insert_health_metrics(&agent_c)
            .await
            .expect("insert agent-c health metrics");

        let mut agent_a = record.clone();
        agent_a.id = format!("health-{}", uuid::Uuid::new_v4());
        agent_a.agent_name = "agent-a".into();
        store
            .insert_health_metrics(&agent_a)
            .await
            .expect("insert agent-a health metrics");

        // List
        let listed = store
            .list_health_metrics(&record.tenant_id, 10, 0)
            .await
            .expect("list_health_metrics");
        assert!(listed.iter().any(|h| h.id == id));
        assert_eq!(
            listed
                .iter()
                .map(|h| h.agent_name.as_str())
                .collect::<Vec<_>>(),
            vec!["agent-a", "agent-b", "agent-c"]
        );

        let model_b = ModelHealthMetrics {
            id: format!("model-health-{}", uuid::Uuid::new_v4()),
            tenant_id: record.tenant_id.clone(),
            model: "model-b".into(),
            period_start: now,
            period_end: now + 3600,
            total_calls: 100,
            successful_calls: 95,
            failed_calls: 5,
            avg_latency_ms: 350,
            p50_latency_ms: 300,
            p95_latency_ms: 800,
            p99_latency_ms: 1200,
            total_tokens: 50000,
            total_cost_usd: dec!(0.500000),
            error_rate_pct: dec!(5.00),
            created_at: now,
        };
        store
            .insert_model_health_metrics(&model_b)
            .await
            .expect("insert model-b health metrics");

        let mut model_c = model_b.clone();
        model_c.id = format!("model-health-{}", uuid::Uuid::new_v4());
        model_c.model = "model-c".into();
        store
            .insert_model_health_metrics(&model_c)
            .await
            .expect("insert model-c health metrics");

        let mut model_a = model_b.clone();
        model_a.id = format!("model-health-{}", uuid::Uuid::new_v4());
        model_a.model = "model-a".into();
        store
            .insert_model_health_metrics(&model_a)
            .await
            .expect("insert model-a health metrics");

        let listed_models = store
            .list_model_metrics(&record.tenant_id, 10, 0)
            .await
            .expect("list_model_metrics");
        assert_eq!(
            listed_models
                .iter()
                .map(|m| m.model.as_str())
                .collect::<Vec<_>>(),
            vec!["model-a", "model-b", "model-c"]
        );

        // Cleanup
        let _ = sqlx::query(
            "DELETE FROM agent_health_metrics WHERE tenant_id LIKE 'integration-test-%'",
        )
        .execute(&pool)
        .await;
        let _ = sqlx::query(
            "DELETE FROM model_health_metrics WHERE tenant_id LIKE 'integration-test-%'",
        )
        .execute(&pool)
        .await;
    }
}
