use crate::db::tenants::UsageSummary;
use crate::db::tenant_agents::{
    AgentTemplate, CreateTenantAgentRequest, TenantAgent, UpdateTenantAgentRequest,
    clone_templates_for_tenant, create_tenant_agent as db_create_tenant_agent,
    delete_tenant_agent as db_delete_tenant_agent, list_agent_templates,
    list_tenant_agents as db_list_tenant_agents,
    update_tenant_agent as db_update_tenant_agent,
};
use crate::db::agent_runs;
use crate::db::alerts as db_alerts;
use crate::db::audit_log;
use crate::llm::provider_registry::ModelInfo;
use crate::models::{Tenant, TenantTier};
use crate::types::{AppError, Result};
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub async fn admin_middleware(
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let admin_secret = std::env::var("ADMIN_API_KEY").ok();

    let header_secret = req
        .headers()
        .get("x-admin-secret")
        .and_then(|v| v.to_str().ok());

    match (admin_secret, header_secret) {
        (Some(expected), Some(given)) if expected == given => {
            next.run(req).await
        }
        _ => {
            Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("Content-Type", "application/json")
                .body(r#"{"error":"Invalid or missing X-Admin-Secret header"}"#.into())
                .unwrap()
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateTenantRequest {
    pub name: String,
    pub tier: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateQuotaRequest {
    pub tier: String,
}

#[derive(Debug, Serialize)]
pub struct TenantResponse {
    pub id: String,
    pub name: String,
    pub tier: String,
    pub created_at: i64,
}

impl From<Tenant> for TenantResponse {
    fn from(t: Tenant) -> Self {
        Self {
            id: t.id,
            name: t.name,
            tier: t.tier.as_str().to_string(),
            created_at: t.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ApiKeyResponse {
    pub id: String,
    pub tenant_id: String,
    pub key_prefix: String,
    pub name: String,
    pub is_active: bool,
    pub created_at: i64,
}

impl From<crate::models::ApiKey> for ApiKeyResponse {
    fn from(k: crate::models::ApiKey) -> Self {
        Self {
            id: k.id,
            tenant_id: k.tenant_id,
            key_prefix: k.key_prefix,
            name: k.name,
            is_active: k.is_active,
            created_at: k.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UsageResponse {
    pub monthly_requests: u64,
    pub monthly_tokens: u64,
    pub daily_requests: u64,
}

impl From<UsageSummary> for UsageResponse {
    fn from(u: UsageSummary) -> Self {
        Self {
            monthly_requests: u.monthly_requests,
            monthly_tokens: u.monthly_tokens,
            daily_requests: u.daily_requests,
        }
    }
}

pub async fn create_tenant(
    State(state): State<AppState>,
    Json(payload): Json<CreateTenantRequest>,
) -> Result<Json<TenantResponse>> {
    let tier = TenantTier::from_str(&payload.tier).ok_or_else(|| {
        AppError::InvalidInput("Invalid tier. Must be: free, dev, pro, or enterprise".to_string())
    })?;

    let tenant = state.tenant_db.create_tenant(payload.name, tier).await?;

    let pool = state.tenant_db.pool().clone();
    let tid = tenant.id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "create_tenant", "tenant", &tid, None, None).await;
    });

    Ok(Json(TenantResponse::from(tenant)))
}

pub async fn list_tenants(
    State(state): State<AppState>,
) -> Result<Json<Vec<TenantResponse>>> {
    let tenants = state.tenant_db.list_tenants().await?;
    let response: Vec<TenantResponse> = tenants.into_iter().map(|t| t.into()).collect();

    Ok(Json(response))
}

pub async fn get_tenant(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<TenantResponse>> {
    let tenant = state.tenant_db.get_tenant(&tenant_id).await?
        .ok_or_else(|| AppError::NotFound("Tenant not found".to_string()))?;

    Ok(Json(TenantResponse::from(tenant)))
}

pub async fn create_api_key(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<Json<serde_json::Value>> {
    let (api_key, raw_key) = state.tenant_db.create_api_key(&tenant_id, payload.name).await?;

    let pool = state.tenant_db.pool().clone();
    let kid = api_key.id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "create_api_key", "api_key", &kid, None, None).await;
    });

    Ok(Json(serde_json::json!({
        "api_key": api_key,
        "raw_key": raw_key,
        "warning": "Store this raw key securely. You will not be able to retrieve it again."
    })))
}

pub async fn list_api_keys(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<ApiKeyResponse>>> {
    let keys = state.tenant_db.list_api_keys(&tenant_id).await?;
    let response: Vec<ApiKeyResponse> = keys.into_iter().map(|k| k.into()).collect();

    Ok(Json(response))
}

pub async fn get_tenant_usage(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<UsageResponse>> {
    let _ = state.tenant_db.get_tenant(&tenant_id).await?
        .ok_or_else(|| AppError::NotFound("Tenant not found".to_string()))?;

    let usage = state.tenant_db.get_usage_summary(&tenant_id).await?;

    Ok(Json(UsageResponse::from(usage)))
}

pub async fn update_tenant_quota(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Json(payload): Json<UpdateQuotaRequest>,
) -> Result<Json<TenantResponse>> {
    let tier = TenantTier::from_str(&payload.tier).ok_or_else(|| {
        AppError::InvalidInput("Invalid tier. Must be: free, dev, pro, or enterprise".to_string())
    })?;

    state.tenant_db.update_tenant_quota(&tenant_id, tier).await?;

    let tenant = state.tenant_db.get_tenant(&tenant_id).await?
        .ok_or_else(|| AppError::NotFound("Tenant not found".to_string()))?;

    let pool = state.tenant_db.pool().clone();
    let tid = tenant_id.clone();
    let details = format!("{{\"new_tier\":\"{}\"}}", payload.tier);
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "update_quota", "tenant", &tid, Some(&details), None).await;
    });

    Ok(Json(TenantResponse::from(tenant)))
}

// =============================================================================
// Provision Client
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct ProvisionClientRequest {
    pub name: String,
    pub tier: String,
    pub product_type: String,
    pub api_key_name: String,
}

#[derive(Debug, Serialize)]
pub struct ProvisionClientResponse {
    pub tenant_id: String,
    pub tenant_name: String,
    pub tier: String,
    pub product_type: String,
    pub api_key_id: String,
    pub api_key_prefix: String,
    pub raw_api_key: String,
    pub agents_created: Vec<String>,
}

pub async fn provision_client(
    State(state): State<AppState>,
    Json(req): Json<ProvisionClientRequest>,
) -> Result<Json<ProvisionClientResponse>> {
    let tier = TenantTier::from_str(&req.tier).ok_or_else(|| {
        AppError::InvalidInput("Invalid tier. Must be: free, dev, pro, or enterprise".to_string())
    })?;

    // product_type is used only to select which agent templates to clone into tenant_agents.
    // It does NOT create product-specific DB tables — client domain data lives in the client's own backend.
    let product_type = req.product_type.to_lowercase();

    let tenant = state.tenant_db.create_tenant(req.name, tier).await?;

    let agents = clone_templates_for_tenant(
        state.tenant_db.pool(),
        &tenant.id,
        &product_type,
    ).await?;

    let (api_key, raw_key) = state.tenant_db.create_api_key(&tenant.id, req.api_key_name).await?;

    let pool = state.tenant_db.pool().clone();
    let tid = tenant.id.clone();
    let details = format!("{{\"product_type\":\"{}\",\"tier\":\"{}\"}}", product_type, tenant.tier.as_str());
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "provision_client", "tenant", &tid, Some(&details), None).await;
    });

    Ok(Json(ProvisionClientResponse {
        tenant_id: tenant.id,
        tenant_name: tenant.name,
        tier: tenant.tier.as_str().to_string(),
        product_type,
        api_key_id: api_key.id,
        api_key_prefix: api_key.key_prefix,
        raw_api_key: raw_key,
        agents_created: agents.into_iter().map(|a| a.agent_name).collect(),
    }))
}

// =============================================================================
// Tenant Agent CRUD
// =============================================================================

pub async fn list_tenant_agents_handler(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<TenantAgent>>> {
    let agents = db_list_tenant_agents(state.tenant_db.pool(), &tenant_id).await?;
    Ok(Json(agents))
}

pub async fn create_tenant_agent_handler(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Json(req): Json<CreateTenantAgentRequest>,
) -> Result<Json<TenantAgent>> {
    let agent = db_create_tenant_agent(state.tenant_db.pool(), &tenant_id, req).await?;

    let pool = state.tenant_db.pool().clone();
    let aid = agent.id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "create_agent", "agent", &aid, None, None).await;
    });

    Ok(Json(agent))
}

pub async fn update_tenant_agent_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    Json(req): Json<UpdateTenantAgentRequest>,
) -> Result<Json<TenantAgent>> {
    let agent = db_update_tenant_agent(state.tenant_db.pool(), &tenant_id, &agent_name, req).await?;

    let pool = state.tenant_db.pool().clone();
    let aid = agent.id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "update_agent", "agent", &aid, None, None).await;
    });

    Ok(Json(agent))
}

pub async fn delete_tenant_agent_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<StatusCode> {
    db_delete_tenant_agent(state.tenant_db.pool(), &tenant_id, &agent_name).await?;

    let pool = state.tenant_db.pool().clone();
    let resource_id = format!("{}:{}", tenant_id, agent_name);
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "delete_agent", "agent", &resource_id, None, None).await;
    });

    Ok(StatusCode::NO_CONTENT)
}

// =============================================================================
// Templates and Models
// =============================================================================

pub async fn list_agent_templates_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<AgentTemplate>>> {
    let product_type = params.get("product_type").map(|s| s.as_str());
    let templates = list_agent_templates(state.tenant_db.pool(), product_type).await?;
    Ok(Json(templates))
}

pub async fn list_models_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<ModelInfo>>> {
    Ok(Json(state.provider_registry.list_models()))
}

// =============================================================================
// Alerts
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct AlertsQuery {
    pub severity: Option<String>,
    pub resolved: Option<bool>,
    pub limit: Option<i64>,
}

pub async fn list_alerts(
    State(state): State<AppState>,
    Query(q): Query<AlertsQuery>,
) -> Result<Json<Vec<db_alerts::Alert>>> {
    let limit = q.limit.unwrap_or(50).min(200);
    let alerts = db_alerts::list_alerts(
        state.tenant_db.pool(),
        q.severity.as_deref(),
        q.resolved,
        limit,
    ).await?;
    Ok(Json(alerts))
}

#[derive(Debug, Deserialize)]
pub struct ResolveAlertRequest {
    pub resolved_by: Option<String>,
}

pub async fn resolve_alert(
    State(state): State<AppState>,
    Path(alert_id): Path<String>,
    Json(payload): Json<ResolveAlertRequest>,
) -> Result<StatusCode> {
    db_alerts::resolve_alert(
        state.tenant_db.pool(),
        &alert_id,
        payload.resolved_by.as_deref(),
    ).await?;

    let pool = state.tenant_db.pool().clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool, "resolve_alert", "alert", &alert_id, None, None,
        ).await;
    });

    Ok(StatusCode::OK)
}

// =============================================================================
// Audit Log
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

pub async fn list_audit_log(
    State(state): State<AppState>,
    Query(q): Query<AuditLogQuery>,
) -> Result<Json<Vec<audit_log::AuditLogEntry>>> {
    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);
    let entries = audit_log::list_audit_log(state.tenant_db.pool(), limit, offset).await?;
    Ok(Json(entries))
}

// =============================================================================
// Daily Usage
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct DailyUsageQuery {
    pub days: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct DailyUsageEntry {
    pub date: i64,
    pub requests: i64,
    pub tokens: i64,
}

pub async fn get_daily_usage(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Query(q): Query<DailyUsageQuery>,
) -> Result<Json<Vec<DailyUsageEntry>>> {
    let days = q.days.unwrap_or(30).min(90);
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let start_ts = now_ts - (days * 86400);

    let rows = sqlx::query(
        "SELECT
            (created_at / 86400) * 86400 as day_ts,
            COUNT(*) as requests,
            COALESCE(SUM(input_tokens + output_tokens), 0) as tokens
         FROM agent_runs
         WHERE tenant_id = $1 AND created_at >= $2
         GROUP BY day_ts ORDER BY day_ts"
    )
    .bind(&tenant_id)
    .bind(start_ts)
    .fetch_all(state.tenant_db.pool())
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    use sqlx::Row;
    let entries: Vec<DailyUsageEntry> = rows.iter().map(|row| {
        DailyUsageEntry {
            date: row.get("day_ts"),
            requests: row.get("requests"),
            tokens: row.get("tokens"),
        }
    }).collect();

    Ok(Json(entries))
}

// =============================================================================
// Agent Runs (Admin view)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct AgentRunsQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

pub async fn list_agent_runs_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    Query(q): Query<AgentRunsQuery>,
) -> Result<Json<Vec<agent_runs::AgentRun>>> {
    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);
    let runs = agent_runs::list_agent_runs(
        state.tenant_db.pool(),
        &tenant_id,
        Some(&agent_name),
        limit,
        offset,
    ).await?;
    Ok(Json(runs))
}

pub async fn get_agent_stats_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<Json<agent_runs::AgentRunStats>> {
    let stats = agent_runs::get_agent_run_stats(
        state.tenant_db.pool(),
        &tenant_id,
        &agent_name,
    ).await?;
    Ok(Json(stats))
}

// =============================================================================
// Cross-tenant agents list
// =============================================================================

pub async fn list_all_agents_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<agent_runs::AllAgentsEntry>>> {
    let agents = agent_runs::list_all_agents(state.tenant_db.pool()).await?;
    Ok(Json(agents))
}

// =============================================================================
// Platform Stats
// =============================================================================

pub async fn get_platform_stats(
    State(state): State<AppState>,
) -> Result<Json<agent_runs::PlatformStats>> {
    let stats = agent_runs::get_platform_stats(state.tenant_db.pool()).await?;
    Ok(Json(stats))
}
