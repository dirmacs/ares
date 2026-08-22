//! Admin billing domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use std::sync::Arc;
use ares_cordis_core::Context;
use super::*;


use crate::AppState;
use crate::types::{AppError, Result};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use sha2::Digest;

pub async fn list_llm_calls(
    State(ctx): State<Arc<Context>>,
    Query(q): Query<ListLlmCallsQuery>,
) -> Result<Json<Vec<RunLlmCall>>> {
    let __pool_1 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_1);
    let calls = store.list_llm_calls(&q).await?;
    Ok(Json(calls))
}

/// Get a single LLM call by id.
pub async fn get_llm_call(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<Json<RunLlmCall>> {
    let __pool_2 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_2);
    let call = store
        .get_llm_call(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("llm call {id} not found")))?;
    Ok(Json(call))
}

/// Insert a new LLM call record.
pub async fn insert_llm_call(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<LogLlmCallRequest>,
) -> Result<Json<RunLlmCall>> {
    let __pool_3 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_3);
    let call = store.insert_llm_call(&req).await?;
    Ok(Json(call))
}

/// List tool calls with optional filtering.
pub async fn list_tool_calls(
    State(ctx): State<Arc<Context>>,
    Query(q): Query<ListToolCallsQuery>,
) -> Result<Json<Vec<RunToolCall>>> {
    let __pool_4 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_4);
    let calls = store.list_tool_calls(&q).await?;
    Ok(Json(calls))
}

/// Get a single tool call by id.
pub async fn get_tool_call(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<Json<RunToolCall>> {
    let __pool_5 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_5);
    let call = store
        .get_tool_call(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("tool call {id} not found")))?;
    Ok(Json(call))
}

/// Insert a new tool call record.
pub async fn insert_tool_call(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<LogToolCallRequest>,
) -> Result<Json<RunToolCall>> {
    let __pool_6 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_6);
    let call = store.insert_tool_call(&req).await?;
    Ok(Json(call))
}

/// Get a run cost by run_id.
pub async fn get_run_cost(
    State(ctx): State<Arc<Context>>,
    Path(run_id): Path<String>,
) -> Result<Json<RunCost>> {
    let __pool_7 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_7);
    let cost = store
        .get_run_cost(&run_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("run cost {run_id} not found")))?;
    Ok(Json(cost))
}

/// List run costs for a tenant.
pub async fn list_run_costs(
    State(ctx): State<Arc<Context>>,
    Query(q): Query<ListRunCostsQuery>,
) -> Result<Json<Vec<RunCost>>> {
    let __pool_8 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_8);
    let limit = q.limit.clamp(1, 10_000);
    let offset = q.offset.max(0);
    let costs = store
        .list_run_costs(
            &q.tenant_id,
            limit,
            offset,
            q.created_after,
            q.created_before,
        )
        .await?;
    Ok(Json(costs))
}

/// Get tenant billing summary for a calendar month.
pub async fn get_tenant_billing_summary(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Query(q): Query<BillingMonthQuery>,
) -> Result<Json<BillingSummaryResponse>> {
    let (month, period_start, period_end) = billing_month_bounds(&q.month)?;
    let __pool_9 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_9);
    let costs = store
        .list_run_costs(&tenant_id, 10_000, 0, Some(period_start), Some(period_end))
        .await?;
    Ok(Json(billing_summary_from_run_costs(
        &tenant_id,
        month,
        period_start,
        period_end,
        &costs,
    )))
}

/// Get tenant billing line items for a calendar month.
pub async fn get_tenant_billing_line_items(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Query(q): Query<BillingMonthQuery>,
) -> Result<Json<BillingLineItemsResponse>> {
    let (month, period_start, period_end) = billing_month_bounds(&q.month)?;
    let __pool_10 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_10);
    let items = store
        .list_run_costs(&tenant_id, 10_000, 0, Some(period_start), Some(period_end))
        .await?
        .iter()
        .map(billing_line_item_from_run_cost)
        .collect();
    Ok(Json(BillingLineItemsResponse {
        tenant_id,
        month,
        items,
    }))
}

/// List configured model billing rates.
pub async fn list_billing_model_rates(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<ModelRateResponse>>> {
    let config = ctx.get::<crate::AresConfigManager>().expect("not provided").config();
    Ok(Json(model_rate_responses(&config.billing)))
}

/// List configured unit billing rates.
pub async fn list_billing_unit_rates() -> Json<Vec<UnitRateResponse>> {
    Json(Vec::new())
}

/// Get a tenant budget.
pub async fn get_tenant_budget(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<TenantBudget>> {
    let __pool_11 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_11);
    let budget = store
        .get_tenant_budget(&tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("tenant budget {tenant_id} not found")))?;
    Ok(Json(budget))
}

/// Set (upsert) a tenant budget.
pub async fn set_tenant_budget(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Json(mut req): Json<SetTenantBudgetRequest>,
) -> Result<Json<TenantBudget>> {
    let __pool_12 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_12);
    req.tenant_id = tenant_id;
    let budget = store.set_tenant_budget(&req).await?;
    Ok(Json(budget))
}

/// Delete a tenant budget.
pub async fn delete_tenant_budget(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let __pool_13 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_13);
    let rows = store.delete_tenant_budget(&tenant_id).await?;
    Ok(Json(
        serde_json::json!({ "deleted": rows > 0, "tenant_id": tenant_id }),
    ))
}

/// Get the enforced token budget for a tenant.
pub async fn get_token_budget(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<TokenBudget>> {
    let __pool_14 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = TokenBudgetStore::new(&__pool_14);
    let budget = store
        .get_budget(&tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("token budget {tenant_id} not found")))?;
    Ok(Json(budget))
}

/// Set the enforced token budget for a tenant.
pub async fn set_token_budget(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Json(req): Json<SetTokenBudgetRequest>,
) -> Result<Json<TokenBudget>> {
    let __pool_15 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = TokenBudgetStore::new(&__pool_15);
    let budget = store
        .set_budget(&tenant_id, req.token_limit, &req.period)
        .await?;
    Ok(Json(budget))
}

/// Get enforced token budget status for a tenant.
pub async fn get_token_budget_status(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<BudgetStatus>> {
    let __pool_16 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = TokenBudgetStore::new(&__pool_16);
    let status = store.check_budget(&tenant_id).await?;
    Ok(Json(status))
}

/// Reset the current enforced token-budget period for a tenant.
pub async fn reset_token_budget_period(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<BudgetStatus>> {
    let __pool_17 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = TokenBudgetStore::new(&__pool_17);
    store.reset_period(&tenant_id).await?;
    let status = store.check_budget(&tenant_id).await?;
    Ok(Json(status))
}

/// List recent enforced token-usage entries for a tenant.
pub async fn list_token_usage(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Query(q): Query<ListTokenUsageQuery>,
) -> Result<Json<Vec<TokenUsageEntry>>> {
    let __pool_18 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = TokenBudgetStore::new(&__pool_18);
    let usage = store
        .list_usage(&tenant_id, q.limit.clamp(1, 10_000))
        .await?;
    Ok(Json(usage))
}

/// List budget alerts with optional filtering.
pub async fn list_budget_alerts(
    State(ctx): State<Arc<Context>>,
    Query(q): Query<ListBudgetAlertsQuery>,
) -> Result<Json<Vec<BudgetAlert>>> {
    let __pool_19 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_19);
    let alerts = store.list_budget_alerts(&q).await?;
    Ok(Json(alerts))
}

/// Acknowledge a budget alert.
pub async fn acknowledge_budget_alert(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    Json(req): Json<AcknowledgeBudgetAlertRequest>,
) -> Result<Json<BudgetAlert>> {
    let __pool_20 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_20);
    let alert = store.acknowledge_budget_alert(&id, &req).await?;
    Ok(Json(alert))
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/billing/list_llm_calls", get(list_llm_calls))
        .route("/billing/get_llm_call", get(get_llm_call))
        .route("/billing/insert_llm_call", post(insert_llm_call))
        .route("/billing/list_tool_calls", get(list_tool_calls))
        .route("/billing/get_tool_call", get(get_tool_call))
        .route("/billing/insert_tool_call", post(insert_tool_call))
        .route("/billing/get_run_cost", get(get_run_cost))
        .route("/billing/list_run_costs", get(list_run_costs))
        .route("/billing/get_tenant_billing_summary", get(get_tenant_billing_summary))
        .route("/billing/get_tenant_billing_line_items", get(get_tenant_billing_line_items))
        .route("/billing/list_billing_model_rates", get(list_billing_model_rates))
        .route("/billing/list_billing_unit_rates", get(list_billing_unit_rates))
        .route("/billing/get_tenant_budget", get(get_tenant_budget))
        .route("/billing/set_tenant_budget", put(set_tenant_budget))
        .route("/billing/delete_tenant_budget", delete(delete_tenant_budget))
        .route("/billing/get_token_budget", get(get_token_budget))
        .route("/billing/set_token_budget", put(set_token_budget))
        .route("/billing/get_token_budget_status", get(get_token_budget_status))
        .route("/billing/reset_token_budget_period", post(reset_token_budget_period))
        .route("/billing/list_token_usage", get(list_token_usage))
        .route("/billing/list_budget_alerts", get(list_budget_alerts))
        .route("/billing/acknowledge_budget_alert", post(acknowledge_budget_alert))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ares_cordis_core::Service;
pub struct AdminBillingService;
impl Service for AdminBillingService {}
