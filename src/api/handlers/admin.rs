use crate::agents::context_provider::AgentRuntimeContext;
use crate::agents::tenant_agent;
use crate::db::agent_runs;
use crate::db::agent_versions;
use crate::db::alerts as db_alerts;
use crate::db::audit_log;
use crate::db::tenant_agents::{
    clone_templates_for_tenant, create_tenant_agent as db_create_tenant_agent,
    delete_tenant_agent as db_delete_tenant_agent, get_tenant_agent as db_get_tenant_agent,
    list_agent_templates, list_tenant_agent_versions, list_tenant_agents as db_list_tenant_agents,
    record_tenant_agent_version, rollback_tenant_agent_version,
    update_tenant_agent as db_update_tenant_agent, AgentTemplate, CreateTenantAgentRequest,
    TenantAgent, UpdateTenantAgentRequest,
};
use crate::db::tenants::UsageSummary;
use crate::llm::provider_registry::ModelInfo;
use crate::memory::estimate_tokens;
use crate::models::{Tenant, TenantTier};
use crate::types::{AgentContext, AppError, Result};
use crate::utils::toml_config::BillingConfig;
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

/// Extended JWT claims that include Eruka's roles map.
#[derive(Debug, Deserialize)]
struct AdminClaims {
    pub sub: String,
    pub email: String,
    pub exp: usize,
    pub iat: usize,
    #[serde(default)]
    pub roles: HashMap<String, Vec<RoleEntry>>,
}

#[derive(Debug, Deserialize)]
struct RoleEntry {
    pub role: String,
    #[allow(dead_code)]
    pub resource_id: Option<String>,
}

/// Check if JWT claims have admin or super_admin role in any of: "admin", "ares", "eruka".
fn has_admin_role(claims: &AdminClaims) -> bool {
    for product in ["admin", "ares", "eruka"] {
        if let Some(entries) = claims.roles.get(product) {
            if entries
                .iter()
                .any(|e| matches!(e.role.as_str(), "admin" | "super_admin"))
            {
                return true;
            }
        }
    }
    false
}

pub async fn admin_middleware(req: axum::extract::Request, next: Next) -> Response {
    // Method 1: X-Admin-Secret header (legacy, backward-compatible)
    let admin_secret = std::env::var("ADMIN_API_KEY").ok();
    let header_secret = req
        .headers()
        .get("x-admin-secret")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    if let (Some(expected), Some(given)) = (&admin_secret, &header_secret) {
        if expected == given {
            return next.run(req).await;
        }
    }

    // Method 2: JWT Bearer token with admin role
    let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_default();
    if !jwt_secret.is_empty() {
        if let Some(token) = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
        {
            let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
            validation.leeway = 60;
            if let Ok(data) = jsonwebtoken::decode::<AdminClaims>(
                token,
                &jsonwebtoken::DecodingKey::from_secret(jwt_secret.as_bytes()),
                &validation,
            ) {
                if has_admin_role(&data.claims) {
                    return next.run(req).await;
                }
            }
        }
    }

    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("Content-Type", "application/json")
        .body(
            r#"{"error":"Admin access requires X-Admin-Secret header or JWT with admin role"}"#
                .into(),
        )
        .unwrap()
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
        let _ =
            audit_log::log_admin_action(&pool, "create_tenant", "tenant", &tid, None, None).await;
    });

    Ok(Json(TenantResponse::from(tenant)))
}

pub async fn list_tenants(State(state): State<AppState>) -> Result<Json<Vec<TenantResponse>>> {
    let tenants = state.tenant_db.list_tenants().await?;
    let response: Vec<TenantResponse> = tenants.into_iter().map(|t| t.into()).collect();

    Ok(Json(response))
}

pub async fn get_tenant(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<TenantResponse>> {
    let tenant = state
        .tenant_db
        .get_tenant(&tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Tenant not found".to_string()))?;

    Ok(Json(TenantResponse::from(tenant)))
}

pub async fn create_api_key(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<Json<serde_json::Value>> {
    let (api_key, raw_key) = state
        .tenant_db
        .create_api_key(&tenant_id, payload.name)
        .await?;

    let pool = state.tenant_db.pool().clone();
    let kid = api_key.id.clone();
    tokio::spawn(async move {
        let _ =
            audit_log::log_admin_action(&pool, "create_api_key", "api_key", &kid, None, None).await;
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
    let _ = state
        .tenant_db
        .get_tenant(&tenant_id)
        .await?
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

    state
        .tenant_db
        .update_tenant_quota(&tenant_id, tier)
        .await?;

    let tenant = state
        .tenant_db
        .get_tenant(&tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Tenant not found".to_string()))?;

    let pool = state.tenant_db.pool().clone();
    let tid = tenant_id.clone();
    let details = format!("{{\"new_tier\":\"{}\"}}", payload.tier);
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "update_quota",
            "tenant",
            &tid,
            Some(&details),
            None,
        )
        .await;
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

    let agents =
        clone_templates_for_tenant(state.tenant_db.pool(), &tenant.id, &product_type).await?;

    let (api_key, raw_key) = state
        .tenant_db
        .create_api_key(&tenant.id, req.api_key_name)
        .await?;

    let pool = state.tenant_db.pool().clone();
    let tid = tenant.id.clone();
    let details = format!(
        "{{\"product_type\":\"{}\",\"tier\":\"{}\"}}",
        product_type,
        tenant.tier.as_str()
    );
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "provision_client",
            "tenant",
            &tid,
            Some(&details),
            None,
        )
        .await;
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
    let agent =
        db_update_tenant_agent(state.tenant_db.pool(), &tenant_id, &agent_name, req).await?;

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
        let _ =
            audit_log::log_admin_action(&pool, "delete_agent", "agent", &resource_id, None, None)
                .await;
    });

    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_tenant_agent_versions_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<Json<Vec<agent_versions::AgentVersionRecord>>> {
    let agent = db_get_tenant_agent(state.tenant_db.pool(), &tenant_id, &agent_name).await?;
    let mut records =
        list_tenant_agent_versions(state.tenant_db.pool(), &tenant_id, &agent_name, 50).await?;
    if records.is_empty() {
        record_tenant_agent_version(state.tenant_db.pool(), &agent, "admin_seed").await?;
        records =
            list_tenant_agent_versions(state.tenant_db.pool(), &tenant_id, &agent_name, 50).await?;
    }
    Ok(Json(records))
}

pub async fn rollback_tenant_agent_version_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name, version)): Path<(String, String, String)>,
) -> Result<Json<TenantAgent>> {
    let agent =
        rollback_tenant_agent_version(state.tenant_db.pool(), &tenant_id, &agent_name, &version)
            .await?;

    let pool = state.tenant_db.pool().clone();
    let resource_id = format!("{}:{}", tenant_id, agent_name);
    let details = format!("Rolled back tenant agent to version {}", version);
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "tenant_agent_rollback",
            "agent",
            &resource_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(agent))
}

#[derive(Debug, Deserialize)]
pub struct TestTenantAgentRequest {
    pub message: String,
    pub config: serde_json::Value,
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub use_eruka_context: bool,
}

#[derive(Debug, Serialize)]
pub struct TestTenantAgentResponse {
    pub status: String,
    pub response: Option<String>,
    pub error: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
    pub model_name: Option<String>,
    pub provider_name: Option<String>,
    pub config_source: String,
    pub config_version: String,
    pub workspace_id: Option<String>,
    pub eruka_context_injected: bool,
}

pub async fn test_tenant_agent_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    Json(req): Json<TestTenantAgentRequest>,
) -> Result<Json<TestTenantAgentResponse>> {
    if state
        .emergency_stop
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        ));
    }

    let message = req.message.trim();
    if message.is_empty() {
        return Err(AppError::InvalidInput(
            "Test Agent requires a non-empty message".to_string(),
        ));
    }

    db_get_tenant_agent(state.tenant_db.pool(), &tenant_id, &agent_name).await?;
    let agent_config = tenant_agent::agent_config_from_json(&req.config)?;
    let draft_agent = state
        .agent_registry
        .create_agent_from_config(&agent_name, &agent_config)
        .await?;

    let agent_context = AgentContext {
        user_id: tenant_id.clone(),
        session_id: format!("admin-test-{}", uuid::Uuid::new_v4()),
        conversation_history: vec![],
        user_memory: None,
    };

    let mut runtime_context =
        AgentRuntimeContext::new(tenant_id.clone(), agent_name.clone(), "admin_test_agent");
    runtime_context.workspace_id = req
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    runtime_context.session_id = Some(agent_context.session_id.clone());

    let eruka_context = if req.use_eruka_context {
        state
            .context_provider
            .get_context_for_run(&runtime_context)
            .await
    } else {
        None
    };
    let eruka_context_injected = eruka_context.is_some();
    let effective_message = if let Some(ctx) = eruka_context {
        format!("{}\n\n---\nUser message: {}", ctx, message)
    } else {
        message.to_string()
    };

    let start = std::time::Instant::now();
    use crate::agents::Agent;
    let result = draft_agent
        .execute(&effective_message, &agent_context)
        .await;
    let duration_ms = start.elapsed().as_millis() as u64;

    let config_version = req
        .config
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("draft:{}", value))
        .unwrap_or_else(|| "draft".to_string());

    match result {
        Ok(response) => {
            let (input_tokens, output_tokens) = if let Some(ref usage) = response.usage {
                (usage.prompt_tokens as u64, usage.completion_tokens as u64)
            } else {
                (
                    estimate_tokens(&effective_message) as u64,
                    estimate_tokens(&response.content) as u64,
                )
            };
            let model_name = response
                .metadata
                .as_ref()
                .map(|metadata| metadata.model_name.clone());
            let provider_name = response
                .metadata
                .as_ref()
                .map(|metadata| metadata.provider_name.clone());

            Ok(Json(TestTenantAgentResponse {
                status: "completed".to_string(),
                response: Some(response.content),
                error: None,
                input_tokens,
                output_tokens,
                duration_ms,
                model_name,
                provider_name,
                config_source: "draft".to_string(),
                config_version,
                workspace_id: runtime_context.workspace_id,
                eruka_context_injected,
            }))
        }
        Err(error) => Ok(Json(TestTenantAgentResponse {
            status: "failed".to_string(),
            response: None,
            error: Some(error.to_string()),
            input_tokens: estimate_tokens(&effective_message) as u64,
            output_tokens: 0,
            duration_ms,
            model_name: None,
            provider_name: None,
            config_source: "draft".to_string(),
            config_version,
            workspace_id: runtime_context.workspace_id,
            eruka_context_injected,
        })),
    }
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

pub async fn list_models_handler(State(state): State<AppState>) -> Result<Json<Vec<ModelInfo>>> {
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
    )
    .await?;
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
    )
    .await?;

    let pool = state.tenant_db.pool().clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "resolve_alert", "alert", &alert_id, None, None)
            .await;
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
            COALESCE(SUM(input_tokens + output_tokens)::bigint, 0) as tokens
         FROM agent_runs
         WHERE tenant_id = $1 AND created_at >= $2
         GROUP BY day_ts ORDER BY day_ts",
    )
    .bind(&tenant_id)
    .bind(start_ts)
    .fetch_all(state.tenant_db.pool())
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

// =============================================================================
// Agent Runs (Admin view)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct AgentRunsQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Estimated cost attached to an admin-visible agent run.
#[derive(Debug, Clone, Serialize)]
pub struct CostEstimateResponse {
    /// Currency for the estimate.
    pub currency: String,
    /// Estimated input-token cost in USD, if pricing is configured.
    pub input_cost_usd: Option<f64>,
    /// Estimated output-token cost in USD, if pricing is configured.
    pub output_cost_usd: Option<f64>,
    /// Estimated total cost in USD, if both input and output rates are configured.
    pub total_cost_usd: Option<f64>,
    /// Whether a matching provider/model pricing entry was found.
    pub pricing_known: bool,
}

impl CostEstimateResponse {
    fn unknown() -> Self {
        Self {
            currency: "USD".to_string(),
            input_cost_usd: None,
            output_cost_usd: None,
            total_cost_usd: None,
            pricing_known: false,
        }
    }
}

/// Admin response for one run plus derived operational metrics.
#[derive(Debug, Clone, Serialize)]
pub struct AgentRunResponse {
    /// Raw persisted run fields.
    #[serde(flatten)]
    pub run: agent_runs::AgentRun,
    /// Estimated cost derived from explicit config pricing.
    pub cost_estimate: CostEstimateResponse,
}

impl AgentRunResponse {
    fn from_run(run: agent_runs::AgentRun, billing: &BillingConfig) -> Self {
        let cost_estimate = estimate_run_cost(billing, &run);
        Self { run, cost_estimate }
    }
}

pub async fn list_agent_runs_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    Query(q): Query<AgentRunsQuery>,
) -> Result<Json<Vec<AgentRunResponse>>> {
    let limit = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);
    let runs = agent_runs::list_agent_runs(
        state.tenant_db.pool(),
        &tenant_id,
        Some(&agent_name),
        limit,
        offset,
    )
    .await?;
    let config = state.config_manager.config();
    let response = runs
        .into_iter()
        .map(|run| AgentRunResponse::from_run(run, &config.billing))
        .collect();
    Ok(Json(response))
}

fn estimate_run_cost(billing: &BillingConfig, run: &agent_runs::AgentRun) -> CostEstimateResponse {
    let Some(pricing) = billing.pricing_for(&run.provider_name, &run.model_name) else {
        return CostEstimateResponse::unknown();
    };

    let input_cost_usd = pricing
        .input_usd_per_million_tokens
        .map(|rate| tokens_to_cost(run.input_tokens, rate));
    let output_cost_usd = pricing
        .output_usd_per_million_tokens
        .map(|rate| tokens_to_cost(run.output_tokens, rate));
    let total_cost_usd = match (input_cost_usd, output_cost_usd) {
        (Some(input), Some(output)) => Some(input + output),
        _ => None,
    };

    CostEstimateResponse {
        currency: pricing.currency.clone(),
        input_cost_usd,
        output_cost_usd,
        total_cost_usd,
        pricing_known: true,
    }
}

fn tokens_to_cost(tokens: i64, usd_per_million_tokens: f64) -> f64 {
    (tokens.max(0) as f64 / 1_000_000.0) * usd_per_million_tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::toml_config::ModelPricingConfig;

    fn run(provider_name: &str, model_name: &str) -> agent_runs::AgentRun {
        agent_runs::AgentRun {
            id: "run-1".into(),
            tenant_id: "tenant-1".into(),
            agent_name: "agent-1".into(),
            user_id: None,
            workspace_id: None,
            session_id: None,
            status: "completed".into(),
            input_tokens: 2_000,
            output_tokens: 500,
            duration_ms: 750,
            error: None,
            created_at: 1_700_000_000,
            model_name: model_name.into(),
            provider_name: provider_name.into(),
            is_streaming: false,
            request_source: Some("api_v1_chat".into()),
            product: None,
            agent_config_source: Some("tenant_db".into()),
            agent_config_version: Some("v1".into()),
            eruka_binding_id: None,
            eruka_context_hit: false,
            eruka_read_count: 0,
            eruka_write_count: 0,
        }
    }

    fn billing() -> BillingConfig {
        let mut billing = BillingConfig::default();
        billing.model_pricing.insert(
            "gpt_test".into(),
            ModelPricingConfig {
                provider: "openai".into(),
                model: "gpt-test".into(),
                input_usd_per_million_tokens: Some(5.0),
                output_usd_per_million_tokens: Some(15.0),
                currency: "USD".into(),
            },
        );
        billing
    }

    #[test]
    fn estimate_run_cost_returns_unknown_without_matching_pricing() {
        let estimate = estimate_run_cost(&BillingConfig::default(), &run("openai", "gpt-test"));

        assert!(!estimate.pricing_known);
        assert_eq!(estimate.total_cost_usd, None);
    }

    #[test]
    fn estimate_run_cost_uses_configured_provider_model_pricing() {
        let estimate = estimate_run_cost(&billing(), &run(" OpenAI ", "GPT-Test"));

        assert!(estimate.pricing_known);
        assert_eq!(estimate.input_cost_usd, Some(0.01));
        assert_eq!(estimate.output_cost_usd, Some(0.0075));
        assert_eq!(estimate.total_cost_usd, Some(0.0175));
    }
}

pub async fn get_agent_stats_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<Json<agent_runs::AgentRunStats>> {
    let stats =
        agent_runs::get_agent_run_stats(state.tenant_db.pool(), &tenant_id, &agent_name).await?;
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

// =============================================================================
// Agent Versioning — Rollback + Kill Switch (Sprint 12)
// =============================================================================

/// GET /api/admin/agents/{agent_id}/versions
/// List all recorded versions for a TOON agent (most recent first).
pub async fn list_agent_versions_handler(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<Json<Vec<agent_versions::AgentVersionRecord>>> {
    let records = agent_versions::get_agent_version_history(state.tenant_db.pool(), &agent_id, 50)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(Json(records))
}

/// POST /api/admin/agents/{agent_id}/rollback/{version}
/// Restore a TOON agent to a specific previously-recorded version.
/// Hot-swaps the in-memory config; writes a new "rollback" row to agent_config_versions.
pub async fn rollback_agent_handler(
    State(state): State<AppState>,
    Path((agent_id, version)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
    // Fetch the target version from DB
    let history = agent_versions::get_agent_version_history(state.tenant_db.pool(), &agent_id, 100)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    let record = history
        .into_iter()
        .find(|r| r.version == version)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "No version '{}' found for agent '{}'",
                version, agent_id
            ))
        })?;

    // Deserialize config_json back to ToonAgentConfig
    let agent_config: crate::utils::toon_config::ToonAgentConfig =
        serde_json::from_value(record.config_json).map_err(|e| {
            AppError::InvalidInput(format!("Failed to deserialize agent config: {}", e))
        })?;

    // Hot-swap into the in-memory DynamicConfigManager
    state.dynamic_config.upsert_agent(agent_config.clone());

    // Record the rollback as a new version entry
    let pool = state.tenant_db.pool().clone();
    let _ = agent_versions::record_agent_versions(&pool, &[agent_config], "rollback").await;

    // Audit log
    let pool2 = state.tenant_db.pool().clone();
    let aid = agent_id.clone();
    let ver = version.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool2,
            "agent_rollback",
            "agent",
            &aid,
            Some(&format!("Rolled back to version {}", ver)),
            None,
        )
        .await;
    });

    tracing::info!(agent_id = %agent_id, version = %version, "Agent rolled back");

    Ok(Json(serde_json::json!({
        "agent_id": agent_id,
        "version": version,
        "status": "rolled_back"
    })))
}

#[derive(Debug, Deserialize)]
pub struct EmergencyStopRequest {
    pub active: bool,
}

/// POST /api/admin/agents/emergency-stop
/// Enable or disable the global emergency stop.
/// When active, ALL /api/v1/chat requests are rejected with 503.
pub async fn emergency_stop_handler(
    State(state): State<AppState>,
    Json(payload): Json<EmergencyStopRequest>,
) -> Result<Json<serde_json::Value>> {
    state
        .emergency_stop
        .store(payload.active, std::sync::atomic::Ordering::Relaxed);

    let action = if payload.active {
        "emergency_stop_enabled"
    } else {
        "emergency_stop_disabled"
    };
    tracing::warn!(active = payload.active, "Emergency stop toggled");

    let pool = state.tenant_db.pool().clone();
    tokio::spawn(async move {
        let _ =
            audit_log::log_admin_action(&pool, action, "platform", "all_agents", None, None).await;
    });

    Ok(Json(serde_json::json!({
        "emergency_stop": payload.active,
        "message": if payload.active {
            "All agents are now in emergency stop mode. /api/v1/chat requests will return 503."
        } else {
            "Emergency stop cleared. Agents are operational."
        }
    })))
}
