//! Admin audit domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use super::*;


use crate::AppState;
use crate::db::agent_feedback;
use crate::db::agent_runs;
use crate::db::alerts as db_alerts;
use crate::db::audit_log;
use crate::db::tenant_allowlist as allowlist;
use crate::types::{AppError, Result};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use sha2::Digest;
use std::sync::Arc;
use ares_cordis_core::Context;

pub async fn list_alerts(
    State(ctx): State<Arc<Context>>,
    Query(q): Query<AlertsQuery>,
) -> Result<Json<Vec<db_alerts::Alert>>> {
    let limit = q.limit.unwrap_or(50).min(200);
    let __pool_1 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let alerts = db_alerts::list_alerts(&__pool_1,
        q.severity.as_deref(),
        q.resolved,
        limit,
    )
    .await?;
    Ok(Json(alerts))
}

pub async fn resolve_alert(
    State(ctx): State<Arc<Context>>,
    Path(alert_id): Path<String>,
    Json(payload): Json<ResolveAlertRequest>,
) -> Result<StatusCode> {
    let __pool_2 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    db_alerts::resolve_alert(&__pool_2,
        &alert_id,
        payload.resolved_by.as_deref(),
    )
    .await?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "resolve_alert", "alert", &alert_id, None, None)
            .await;
    });

    Ok(StatusCode::OK)
}

pub async fn list_audit_log(
    State(ctx): State<Arc<Context>>,
    Query(q): Query<AuditLogQuery>,
) -> Result<Json<Vec<audit_log::AuditLogEntry>>> {
    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);
    let __pool_3 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let entries = audit_log::list_audit_log(&__pool_3, limit, offset).await?;
    Ok(Json(entries))
}

pub async fn get_daily_usage(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Query(q): Query<DailyUsageQuery>,
) -> Result<Json<Vec<DailyUsageEntry>>> {
    let days = q.days.unwrap_or(30).min(90);
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let start_ts = now_ts - (days * 86400);

    let __pool_4 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let rows = sqlx::query(
        "SELECT
            (created_at / 86400) * 86400 as day_ts,
            COUNT(*) as requests,
            COALESCE(SUM(input_tokens + output_tokens)::bigint, 0) as tokens
         FROM agent_runs
         WHERE tenant_id = $1 AND created_at >= $2
         GROUP BY day_ts ORDER BY day_ts",
    )
    .bind(&tenant_id)
    .bind(start_ts)
    .fetch_all(&__pool_4)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    use sqlx::Row;
    let entries: Vec<DailyUsageEntry> = rows
        .iter()
        .map(|row| DailyUsageEntry {
            date: row.get("day_ts"),
            requests: row.get("requests"),
            tokens: row.get("tokens"),
        })
        .collect();

    Ok(Json(entries))
}

pub async fn list_agent_runs_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    Query(q): Query<AgentRunsQuery>,
) -> Result<Json<Vec<AgentRunResponse>>> {
    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);
    let __pool_5 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let runs = agent_runs::list_agent_runs(&__pool_5,
        &tenant_id,
        Some(&agent_name),
        limit,
        offset,
    )
    .await?;
    let config = ctx.get::<crate::AresConfigManager>().expect("not provided").config();
    let response = runs
        .into_iter()
        .map(|run| AgentRunResponse::from_run(run, &config.billing))
        .collect();
    Ok(Json(response))
}

pub async fn create_agent_run_feedback_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name, run_id)): Path<(String, String, String)>,
    Json(payload): Json<CreateAgentRunFeedbackRequest>,
) -> Result<Json<agent_feedback::AgentRunFeedback>> {
    let __pool_6 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let feedback = agent_feedback::insert_agent_run_feedback(&__pool_6,
        agent_feedback::NewAgentRunFeedback {
            tenant_id: tenant_id.clone(),
            agent_name: agent_name.clone(),
            run_id: Some(run_id.clone()),
            feedback_type: payload.feedback_type,
            score: payload.score,
            flags: payload.flags,
            notes: payload.notes,
            reviewer: payload.reviewer,
        },
    )
    .await?;

    let feedback_id = feedback.id.clone();
    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    tokio::spawn(async move {
        let details = serde_json::json!({
            "agent_name": agent_name,
            "run_id": run_id,
            "feedback_id": feedback_id,
        })
        .to_string();
        let _ = audit_log::log_admin_action(
            &pool,
            "agent_run_feedback",
            "agent_run",
            &tenant_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(feedback))
}

pub async fn get_agent_feedback_summary_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    Query(q): Query<AgentFeedbackSummaryQuery>,
) -> Result<Json<agent_feedback::AgentFeedbackSummary>> {
    let days = q.days.unwrap_or(30).clamp(1, 366);
    let __pool_7 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let summary = agent_feedback::get_agent_feedback_summary(&__pool_7,
        &tenant_id,
        &agent_name,
        days,
    )
    .await?;
    Ok(Json(summary))
}

pub async fn list_tenant_allowed_tools(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<allowlist::TenantToolAllowlistItem>>> {
    let __pool_8 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = allowlist::TenantAllowlistStore::new(&__pool_8);
    let items = store.list_tools(&tenant_id).await?;
    Ok(Json(items))
}

pub async fn add_tenant_allowed_tool(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Json(req): Json<AllowToolRequest>,
) -> Result<Json<allowlist::TenantToolAllowlistItem>> {
    let __pool_9 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = allowlist::TenantAllowlistStore::new(&__pool_9);
    let item = store.allow_tool(&tenant_id, &req.tool_name).await?;
    Ok(Json(item))
}

pub async fn delete_tenant_allowed_tool(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, tool_name)): Path<(String, String)>,
) -> Result<StatusCode> {
    let __pool_10 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = allowlist::TenantAllowlistStore::new(&__pool_10);
    let rows = store.deny_tool(&tenant_id, &tool_name).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "tool {} not found for tenant {}",
            tool_name, tenant_id
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_tenant_allowed_models(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<allowlist::TenantModelAllowlistItem>>> {
    let __pool_11 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = allowlist::TenantAllowlistStore::new(&__pool_11);
    let items = store.list_models(&tenant_id).await?;
    Ok(Json(items))
}

pub async fn add_tenant_allowed_model(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Json(req): Json<AllowModelRequest>,
) -> Result<Json<allowlist::TenantModelAllowlistItem>> {
    let __pool_12 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = allowlist::TenantAllowlistStore::new(&__pool_12);
    let item = store.allow_model(&tenant_id, &req.model_id).await?;
    Ok(Json(item))
}

pub async fn delete_tenant_allowed_model(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, model_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let __pool_13 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = allowlist::TenantAllowlistStore::new(&__pool_13);
    let rows = store.deny_model(&tenant_id, &model_id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "model {} not found for tenant {}",
            model_id, tenant_id
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_tenant_allowed_rag_sources(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<allowlist::TenantRagAllowlistItem>>> {
    let __pool_14 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = allowlist::TenantAllowlistStore::new(&__pool_14);
    let items = store.list_rag_sources(&tenant_id).await?;
    Ok(Json(items))
}

pub async fn add_tenant_allowed_rag_source(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Json(req): Json<AllowRagSourceRequest>,
) -> Result<Json<allowlist::TenantRagAllowlistItem>> {
    let __pool_15 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = allowlist::TenantAllowlistStore::new(&__pool_15);
    let item = store.allow_rag_source(&tenant_id, &req.rag_source).await?;
    Ok(Json(item))
}

pub async fn delete_tenant_allowed_rag_source(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, rag_source)): Path<(String, String)>,
) -> Result<StatusCode> {
    let __pool_16 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = allowlist::TenantAllowlistStore::new(&__pool_16);
    let rows = store.deny_rag_source(&tenant_id, &rag_source).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "rag source {} not found for tenant {}",
            rag_source, tenant_id
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_agent_stats_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<Json<agent_runs::AgentRunStats>> {
        let __pool_17 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let stats = agent_runs::get_agent_run_stats(&__pool_17, &tenant_id, &agent_name).await?;
    Ok(Json(stats))
}

pub async fn list_all_agents_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<agent_runs::AllAgentsEntry>>> {
    let __pool_18 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let agents = agent_runs::list_all_agents(&__pool_18).await?;
    Ok(Json(agents))
}

pub async fn get_platform_stats(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<agent_runs::PlatformStats>> {
    let __pool_19 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let stats = agent_runs::get_platform_stats(&__pool_19).await?;
    Ok(Json(stats))
}

/// POST /api/webhooks/{trigger_id}

/// GET /api/admin/runs/live — SSE stream of active agent runs
pub async fn stream_active_runs(
    State(ctx): State<Arc<Context>>,
) -> axum::response::Sse<
    impl futures::Stream<
        Item = std::result::Result<axum::response::sse::Event, std::convert::Infallible>,
    >,
> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use std::time::Duration;
    use tokio::time::interval;

    let active_runs = Arc::clone(&ctx.get::<crate::context_services::ActiveRunsService>().expect("not provided").0);
    let stream = futures::stream::unfold(interval(Duration::from_secs(2)), move |mut interval| {
        let active_runs = Arc::clone(&active_runs);
        async move {
            interval.tick().await;
            let runs = active_runs.list();
            let data = serde_json::json!({
                "timestamp": chrono::Utc::now().timestamp(),
                "runs": runs,
                "count": runs.len(),
            });
            let event = Ok(Event::default().data(data.to_string()));
            Some((event, interval))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(30))
            .text("keep-alive"),
    )
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/audit/list_alerts", get(list_alerts))
        .route("/audit/resolve_alert", post(resolve_alert))
        .route("/audit/list_audit_log", get(list_audit_log))
        .route("/audit/get_daily_usage", get(get_daily_usage))
        .route("/audit/list_agent_runs_handler", get(list_agent_runs_handler))
        .route("/audit/create_agent_run_feedback_handler", post(create_agent_run_feedback_handler))
        .route("/audit/get_agent_feedback_summary_handler", get(get_agent_feedback_summary_handler))
        .route("/audit/list_tenant_allowed_tools", get(list_tenant_allowed_tools))
        .route("/audit/add_tenant_allowed_tool", post(add_tenant_allowed_tool))
        .route("/audit/delete_tenant_allowed_tool", delete(delete_tenant_allowed_tool))
        .route("/audit/list_tenant_allowed_models", get(list_tenant_allowed_models))
        .route("/audit/add_tenant_allowed_model", post(add_tenant_allowed_model))
        .route("/audit/delete_tenant_allowed_model", delete(delete_tenant_allowed_model))
        .route("/audit/list_tenant_allowed_rag_sources", get(list_tenant_allowed_rag_sources))
        .route("/audit/add_tenant_allowed_rag_source", post(add_tenant_allowed_rag_source))
        .route("/audit/delete_tenant_allowed_rag_source", delete(delete_tenant_allowed_rag_source))
        .route("/audit/get_agent_stats_handler", get(get_agent_stats_handler))
        .route("/audit/list_all_agents_handler", get(list_all_agents_handler))
        .route("/audit/get_platform_stats", get(get_platform_stats))
        .route("/audit/receive_webhook", post(receive_webhook))
        .route("/audit/stream_active_runs", get(stream_active_runs))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ares_cordis_core::Service;
pub struct AdminAuditService;
impl Service for AdminAuditService {}
