//! V1 API handlers — tenant-scoped endpoints authenticated via API key.
//!
//! These endpoints are called by enterprise-portal and other client apps
//! using `Authorization: Bearer ares_xxx`. The `api_key_auth_middleware`
//! injects `TenantContext` into request extensions before these handlers run.

use crate::db::tenant_agents::{self, TenantAgent};
use crate::db::agent_runs;
use crate::models::TenantContext;
use crate::types::{AppError, Result};
use crate::AppState;
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};

// =============================================================================
// Response types — designed to match enterprise-portal's expected types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct V1Agent {
    pub id: String,
    pub name: String,
    pub agent_type: String,
    pub status: V1AgentStatus,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub total_runs: u64,
    pub success_rate: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum V1AgentStatus {
    Active,
    Idle,
    Error,
    Disabled,
}

impl From<TenantAgent> for V1Agent {
    fn from(a: TenantAgent) -> Self {
        let status = if a.enabled {
            V1AgentStatus::Active
        } else {
            V1AgentStatus::Disabled
        };
        Self {
            id: a.id,
            name: a.agent_name,
            agent_type: "custom".to_string(),
            status,
            config: a.config,
            created_at: ts_to_dt(a.created_at),
            last_run: None,
            total_runs: 0,
            success_rate: 0.0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct V1AgentRun {
    pub id: String,
    pub agent_id: String,
    pub status: String,
    pub input: serde_json::Value,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub tokens_used: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct V1AgentLog {
    pub id: String,
    pub agent_id: String,
    pub run_id: Option<String>,
    pub level: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub per_page: u32,
    pub total_pages: u32,
}

impl<T> Paginated<T> {
    fn empty(page: u32, per_page: u32) -> Self {
        Self {
            items: vec![],
            total: 0,
            page,
            per_page,
            total_pages: 0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct V1Usage {
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub total_runs: u64,
    pub total_tokens: u64,
    pub total_api_calls: u64,
    pub quota_runs: Option<u64>,
    pub quota_tokens: Option<u64>,
    pub daily_usage: Vec<DailyUsage>,
}

#[derive(Debug, Serialize)]
pub struct DailyUsage {
    pub date: String,
    pub runs: u64,
    pub tokens: u64,
    pub api_calls: u64,
}

#[derive(Debug, Serialize)]
pub struct V1ApiKey {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    pub last_used: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub expires_in_days: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct CreateApiKeyResponse {
    pub key: V1ApiKey,
    pub secret: String,
}

#[derive(Debug, Deserialize)]
pub struct PaginationQuery {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

// =============================================================================
// Helpers
// =============================================================================

fn ts_to_dt(ts: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now)
}

fn extract_tenant(ctx: Option<Extension<TenantContext>>) -> Result<TenantContext> {
    ctx.map(|Extension(c)| c)
        .ok_or_else(|| AppError::Auth("Missing tenant context".to_string()))
}

// =============================================================================
// Handlers
// =============================================================================

/// GET /v1/agents — list all agents for this tenant
pub async fn list_agents(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1Agent>>> {
    let tc = extract_tenant(ctx)?;
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(20).min(100);

    let agents = tenant_agents::list_tenant_agents(state.tenant_db.pool(), &tc.tenant_id).await?;
    let total = agents.len() as u64;
    let total_pages = ((total as f64) / (per_page as f64)).ceil() as u32;

    let start = ((page - 1) * per_page) as usize;
    let items: Vec<V1Agent> = agents
        .into_iter()
        .skip(start)
        .take(per_page as usize)
        .map(V1Agent::from)
        .collect();

    Ok(Json(Paginated {
        items,
        total,
        page,
        per_page,
        total_pages,
    }))
}

/// GET /v1/agents/{name} — get a specific agent
pub async fn get_agent(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Path(name): Path<String>,
) -> Result<Json<V1Agent>> {
    let tc = extract_tenant(ctx)?;
    let agent = tenant_agents::get_tenant_agent(state.tenant_db.pool(), &tc.tenant_id, &name).await?;
    Ok(Json(V1Agent::from(agent)))
}

/// POST /v1/agents/{name}/run — trigger an agent run (proxies to chat)
pub async fn run_agent(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Path(name): Path<String>,
    Json(input): Json<serde_json::Value>,
) -> Result<Json<V1AgentRun>> {
    let tc = extract_tenant(ctx)?;
    // Verify the agent exists for this tenant
    let _agent = tenant_agents::get_tenant_agent(state.tenant_db.pool(), &tc.tenant_id, &name).await?;

    // Return a stub run for now — actual execution would proxy through the chat handler
    Ok(Json(V1AgentRun {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: name,
        status: "completed".to_string(),
        input,
        output: Some(serde_json::json!({"message": "Agent run queued"})),
        error: None,
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
        duration_ms: Some(0),
        tokens_used: Some(0),
    }))
}

/// GET /v1/agents/{name}/runs — list runs for an agent
pub async fn list_agent_runs(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Path(name): Path<String>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1AgentRun>>> {
    let tc = extract_tenant(ctx)?;
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(25).min(100);
    let offset = ((page - 1) * per_page) as i64;

    let runs = agent_runs::list_agent_runs(
        state.tenant_db.pool(),
        &tc.tenant_id,
        Some(&name),
        per_page as i64,
        offset,
    ).await?;

    let items: Vec<V1AgentRun> = runs.into_iter().map(|r| V1AgentRun {
        id: r.id,
        agent_id: r.agent_name,
        status: r.status,
        input: serde_json::json!({"tokens": r.input_tokens}),
        output: Some(serde_json::json!({"tokens": r.output_tokens})),
        error: r.error,
        started_at: ts_to_dt(r.created_at),
        finished_at: Some(ts_to_dt(r.created_at + (r.duration_ms / 1000))),
        duration_ms: Some(r.duration_ms as u64),
        tokens_used: Some((r.input_tokens + r.output_tokens) as u64),
    }).collect();

    let total = items.len() as u64;
    Ok(Json(Paginated {
        items,
        total,
        page,
        per_page,
        total_pages: ((total as f64) / (per_page as f64)).ceil() as u32,
    }))
}

/// GET /v1/agents/{name}/logs — list logs for an agent (stub: returns empty)
pub async fn list_agent_logs(
    ctx: Option<Extension<TenantContext>>,
    Path(name): Path<String>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1AgentLog>>> {
    let _tc = extract_tenant(ctx)?;
    let page = q.page.unwrap_or(1);
    let per_page = q.per_page.unwrap_or(50);
    let _ = name;
    Ok(Json(Paginated::empty(page, per_page)))
}

/// GET /v1/usage — get usage summary for this tenant
pub async fn get_usage(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
) -> Result<Json<V1Usage>> {
    let tc = extract_tenant(ctx)?;
    let summary = state.tenant_db.get_usage_summary(&tc.tenant_id).await?;

    let now = Utc::now();
    let period_start = now
        .date_naive()
        .with_day(1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();

    // Quota limits (cap u64::MAX to None for display)
    let quota_runs = if tc.quota.requests_per_month == u64::MAX {
        None
    } else {
        Some(tc.quota.requests_per_month)
    };
    let quota_tokens = if tc.quota.tokens_per_month == u64::MAX {
        None
    } else {
        Some(tc.quota.tokens_per_month)
    };

    Ok(Json(V1Usage {
        period_start,
        period_end: now,
        total_runs: summary.monthly_requests,
        total_tokens: summary.monthly_tokens,
        total_api_calls: summary.monthly_requests,
        quota_runs,
        quota_tokens,
        daily_usage: vec![],
    }))
}

/// GET /v1/api-keys — list API keys for this tenant
pub async fn list_api_keys(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
) -> Result<Json<Vec<V1ApiKey>>> {
    let tc = extract_tenant(ctx)?;
    let keys = state.tenant_db.list_api_keys(&tc.tenant_id).await?;

    let response: Vec<V1ApiKey> = keys
        .into_iter()
        .filter(|k| k.is_active)
        .map(|k| V1ApiKey {
            id: k.id,
            name: k.name,
            prefix: k.key_prefix,
            created_at: ts_to_dt(k.created_at),
            last_used: None,
            expires_at: k.expires_at.map(|e| ts_to_dt(e)),
        })
        .collect();

    Ok(Json(response))
}

/// POST /v1/api-keys — create a new API key
pub async fn create_api_key(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>> {
    let tc = extract_tenant(ctx)?;
    let (api_key, raw_key) = state
        .tenant_db
        .create_api_key(&tc.tenant_id, payload.name)
        .await?;

    Ok(Json(CreateApiKeyResponse {
        key: V1ApiKey {
            id: api_key.id,
            name: api_key.name,
            prefix: api_key.key_prefix,
            created_at: ts_to_dt(api_key.created_at),
            last_used: None,
            expires_at: api_key.expires_at.map(|e| ts_to_dt(e)),
        },
        secret: raw_key,
    }))
}

/// DELETE /v1/api-keys/{id} — revoke an API key
pub async fn revoke_api_key(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Path(key_id): Path<String>,
) -> Result<StatusCode> {
    let tc = extract_tenant(ctx)?;
    state
        .tenant_db
        .revoke_api_key(&tc.tenant_id, &key_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
