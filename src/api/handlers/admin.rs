use crate::agents::context_provider::AgentRuntimeContext;
use crate::agents::tenant_agent;
use crate::db::agent_feedback;
use crate::db::agent_runs;
use crate::db::agent_versions;
use crate::db::alerts as db_alerts;
use crate::db::audit_log;
use crate::db::tenant_model_tiers as db_tiers;
use crate::db::tenant_agents::{
    clone_templates_for_tenant, create_tenant_agent as db_create_tenant_agent,
    delete_tenant_agent as db_delete_tenant_agent, get_tenant_agent as db_get_tenant_agent,
    list_agent_templates, list_all_tenant_agents, list_tenant_agent_versions,
    list_tenant_agents as db_list_tenant_agents, record_tenant_agent_version,
    rollback_tenant_agent_version, update_tenant_agent as db_update_tenant_agent,
    AgentTemplate, AgentTemplateStore, CreateTemplateRequest, CreateTenantAgentRequest,
    TenantAgent, UpdateTenantAgentRequest,
};
use crate::db::tenants::UsageSummary;
use crate::llm::provider_registry::ModelInfo;
use ares_config::toml_config::ProviderConfig;
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
use std::sync::Arc;

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


const INVALID_TIER_MSG: &str = "Invalid tier. Must be: free, dev, pro, or enterprise";

/// Validates a tier string from admin request payloads.
fn parse_tenant_tier(tier: &str) -> Result<TenantTier> {
    TenantTier::from_str(tier).ok_or_else(|| AppError::InvalidInput(INVALID_TIER_MSG.to_string()))
}

pub async fn create_tenant(
    State(state): State<AppState>,
    Json(payload): Json<CreateTenantRequest>,
) -> Result<Json<TenantResponse>> {
    let tier = parse_tenant_tier(&payload.tier)?;

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
    let tier = parse_tenant_tier(&payload.tier)?;

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
    let tier = parse_tenant_tier(&req.tier)?;

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

// =============================================================================
// Cross-tenant Agent CRUD (admin-facing)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub tenant_id: String,
    pub agent_name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub config: serde_json::Value,
    pub template_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub config: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

pub async fn list_agents(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<TenantAgent>>> {
    let tenant_id = params.get("tenant_id").map(|s| s.as_str());
    let agents = list_all_tenant_agents(state.tenant_db.pool(), tenant_id).await?;
    Ok(Json(agents))
}

pub async fn get_agent(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<Json<TenantAgent>> {
    let agent = db_get_tenant_agent(state.tenant_db.pool(), &tenant_id, &agent_name).await?;
    Ok(Json(agent))
}

pub async fn create_agent(
    State(state): State<AppState>,
    Json(req): Json<CreateAgentRequest>,
) -> Result<Json<TenantAgent>> {
    let config = if let Some(tpl_id) = &req.template_id {
        let store = AgentTemplateStore::new(state.tenant_db.pool().clone());
        let tpl = store
            .get_template(tpl_id)
            .await?
            .ok_or_else(|| AppError::InvalidInput(format!("Template '{}' not found", tpl_id)))?;
        tpl.config
    } else {
        req.config
    };

    let db_req = CreateTenantAgentRequest {
        agent_name: req.agent_name,
        display_name: req.display_name,
        description: req.description,
        config,
    };

    let agent = db_create_tenant_agent(state.tenant_db.pool(), &req.tenant_id, db_req).await?;

    let pool = state.tenant_db.pool().clone();
    let aid = agent.id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "create_agent", "agent", &aid, None, None).await;
    });

    Ok(Json(agent))
}

pub async fn update_agent(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    Json(req): Json<UpdateAgentRequest>,
) -> Result<Json<TenantAgent>> {
    let db_req = UpdateTenantAgentRequest {
        display_name: req.display_name,
        description: req.description,
        config: req.config,
        enabled: req.enabled,
    };
    let agent = db_update_tenant_agent(state.tenant_db.pool(), &tenant_id, &agent_name, db_req).await?;

    let pool = state.tenant_db.pool().clone();
    let aid = agent.id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(&pool, "update_agent", "agent", &aid, None, None).await;
    });

    Ok(Json(agent))
}

pub async fn delete_agent(
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

pub async fn get_agent_versions(
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

pub async fn rollback_agent(
    State(state): State<AppState>,
    Path((tenant_id, agent_name, version)): Path<(String, String, String)>,
) -> Result<Json<TenantAgent>> {
    let agent =
        rollback_tenant_agent_version(state.tenant_db.pool(), &tenant_id, &agent_name, &version)
            .await?;

    let pool = state.tenant_db.pool().clone();
    let resource_id = format!("{}:{}", tenant_id, agent_name);
    let details = format!("Rolled back agent to version {}", version);
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "agent_rollback",
            "agent",
            &resource_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(agent))
}

// =============================================================================
// Agent Template CRUD (admin-facing)
// =============================================================================

pub async fn create_agent_template_handler(
    State(state): State<AppState>,
    Json(req): Json<CreateTemplateRequest>,
) -> Result<Json<AgentTemplate>> {
    let store = AgentTemplateStore::new(state.tenant_db.pool().clone());
    let tpl = store.create_template(&req).await?;

    let pool = state.tenant_db.pool().clone();
    let tid = tpl.id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "create_agent_template",
            "agent_template",
            &tid,
            None,
            None,
        )
        .await;
    });

    Ok(Json(tpl))
}

pub async fn delete_agent_template_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let store = AgentTemplateStore::new(state.tenant_db.pool().clone());
    let deleted = store.delete_template(&id).await?;
    if deleted == 0 {
        return Err(AppError::NotFound(format!("Template '{}' not found", id)));
    }

    let pool = state.tenant_db.pool().clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "delete_agent_template",
            "agent_template",
            &id,
            None,
            None,
        )
        .await;
    });

    Ok(StatusCode::NO_CONTENT)
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
    let mut draft_agent = state
        .agent_registry
        .create_agent_from_config(&agent_name, &agent_config)
        .await?;

    // Attach observability
    let run_id = uuid::Uuid::new_v4().to_string();
    let obs = Arc::new(crate::observability::RunObservability {
        run_id: run_id.clone(),
        tenant_id: tenant_id.clone(),
        agent_name: agent_name.clone(),
        pool: state.tenant_db.pool().clone(),
    });
    draft_agent.set_observability(obs.clone());

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

    // Aggregate run costs (fire-and-forget)
    let dur_i64 = duration_ms as i64;
    let obs_for_spawn = obs.clone();
    tokio::spawn(async move {
        obs_for_spawn.aggregate_run_cost(dur_i64).await;
    });

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

/// Query for tenant-agent feedback summaries.
#[derive(Debug, Deserialize)]
pub struct AgentFeedbackSummaryQuery {
    pub days: Option<i64>,
}

/// Request body for recording reviewer quality feedback on one run.
#[derive(Debug, Deserialize)]
pub struct CreateAgentRunFeedbackRequest {
    pub feedback_type: String,
    pub score: Option<f64>,
    #[serde(default)]
    pub flags: Vec<String>,
    pub notes: Option<String>,
    pub reviewer: Option<String>,
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

pub async fn create_agent_run_feedback_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name, run_id)): Path<(String, String, String)>,
    Json(payload): Json<CreateAgentRunFeedbackRequest>,
) -> Result<Json<agent_feedback::AgentRunFeedback>> {
    let feedback = agent_feedback::insert_agent_run_feedback(
        state.tenant_db.pool(),
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
    let pool = state.tenant_db.pool().clone();
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
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    Query(q): Query<AgentFeedbackSummaryQuery>,
) -> Result<Json<agent_feedback::AgentFeedbackSummary>> {
    let days = q.days.unwrap_or(30).clamp(1, 366);
    let summary = agent_feedback::get_agent_feedback_summary(
        state.tenant_db.pool(),
        &tenant_id,
        &agent_name,
        days,
    )
    .await?;
    Ok(Json(summary))
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

    fn admin_claims(roles: HashMap<String, Vec<RoleEntry>>) -> AdminClaims {
        AdminClaims {
            sub: "admin-user".into(),
            email: "admin@example.com".into(),
            exp: 9_999_999_999,
            iat: 1_700_000_000,
            roles,
        }
    }

    fn role_entry(role: &str) -> RoleEntry {
        RoleEntry {
            role: role.into(),
            resource_id: None,
        }
    }

    #[test]
    fn has_admin_role_accepts_super_admin_in_ares_product() {
        let mut roles = HashMap::new();
        roles.insert("ares".into(), vec![role_entry("super_admin")]);
        assert!(has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn has_admin_role_accepts_admin_in_eruka_product() {
        let mut roles = HashMap::new();
        roles.insert("eruka".into(), vec![role_entry("admin")]);
        assert!(has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn has_admin_role_rejects_non_admin_roles() {
        let mut roles = HashMap::new();
        roles.insert("admin".into(), vec![role_entry("viewer")]);
        assert!(!has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn tokens_to_cost_clamps_negative_tokens_to_zero() {
        assert_eq!(tokens_to_cost(-100, 10.0), 0.0);
    }

    #[test]
    fn tokens_to_cost_scales_per_million() {
        let cost = tokens_to_cost(2_000_000, 5.0);
        assert!((cost - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_estimate_unknown_serializes_pricing_flag() {
        let estimate = CostEstimateResponse::unknown();
        let json = serde_json::to_value(&estimate).unwrap();
        assert_eq!(json["pricing_known"], false);
        assert!(json["total_cost_usd"].is_null());
    }

    #[test]
    fn agent_run_response_from_run_attaches_cost_estimate() {
        let response = AgentRunResponse::from_run(run("openai", "gpt-test"), &billing());
        assert!(response.cost_estimate.pricing_known);
        assert_eq!(response.run.id, "run-1");
    }

    #[test]
    fn create_tenant_request_roundtrip() {
        let req = CreateTenantRequest {
            name: "Acme".into(),
            tier: "pro".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CreateTenantRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "Acme");
        assert_eq!(back.tier, "pro");
    }

    #[test]
    fn tenant_response_from_tenant_maps_tier_string() {
        use crate::models::{Tenant, TenantTier};
        let tenant = Tenant::new("t-1".into(), "Acme".into(), TenantTier::Pro);
        let resp = TenantResponse::from(tenant);
        assert_eq!(resp.id, "t-1");
        assert_eq!(resp.tier, "pro");
    }

    #[test]
    fn api_key_response_from_model_maps_prefix() {
        use crate::models::ApiKey;
        let key = ApiKey::new(
            "key-1".into(),
            "tenant-1".into(),
            "hash".into(),
            "ares_ab".into(),
            "Primary".into(),
        );
        let resp = ApiKeyResponse::from(key);
        assert_eq!(resp.key_prefix, "ares_ab");
        assert!(resp.is_active);
    }

    #[test]
    fn usage_response_from_summary_copies_counters() {
        let summary = UsageSummary {
            monthly_requests: 10,
            monthly_tokens: 20,
            daily_requests: 3,
        };
        let resp = UsageResponse::from(summary);
        assert_eq!(resp.monthly_requests, 10);
        assert_eq!(resp.monthly_tokens, 20);
        assert_eq!(resp.daily_requests, 3);
    }

    #[test]
    fn emergency_stop_request_deserializes_active_flag() {
        let req: EmergencyStopRequest = serde_json::from_str(r#"{"active":true}"#).unwrap();
        assert!(req.active);
    }

    #[test]
    fn create_api_key_request_roundtrip() {
        let req = CreateApiKeyRequest {
            name: "Primary".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CreateApiKeyRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "Primary");
    }

    #[test]
    fn update_quota_request_roundtrip() {
        let req = UpdateQuotaRequest {
            tier: "enterprise".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: UpdateQuotaRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tier, "enterprise");
    }

    #[test]
    fn admin_claims_deserializes_roles_map() {
        let json = r#"{
            "sub":"user-1",
            "email":"admin@example.com",
            "exp":9999999999,
            "iat":1700000000,
            "roles":{
                "ares":[{"role":"admin","resource_id":null}]
            }
        }"#;
        let claims: AdminClaims = serde_json::from_str(json).unwrap();
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.email, "admin@example.com");
        assert!(has_admin_role(&claims));
    }

    #[test]
    fn admin_claims_default_empty_roles_when_omitted() {
        let json = r#"{
            "sub":"user-2",
            "email":"viewer@example.com",
            "exp":9999999999,
            "iat":1700000000
        }"#;
        let claims: AdminClaims = serde_json::from_str(json).unwrap();
        assert!(claims.roles.is_empty());
        assert!(!has_admin_role(&claims));
    }

    #[test]
    fn has_admin_role_accepts_admin_in_admin_product() {
        let mut roles = HashMap::new();
        roles.insert("admin".into(), vec![role_entry("admin")]);
        assert!(has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn parse_tenant_tier_accepts_case_insensitive_values() {
        assert!(matches!(parse_tenant_tier("PRO").unwrap(), TenantTier::Pro));
        assert!(matches!(
            parse_tenant_tier("Enterprise").unwrap(),
            TenantTier::Enterprise
        ));
    }

    #[test]
    fn parse_tenant_tier_rejects_unknown_tier_with_invalid_input() {
        let err = parse_tenant_tier("platinum").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains(INVALID_TIER_MSG));
    }

    #[test]
    fn tokens_to_cost_zero_tokens_returns_zero() {
        assert_eq!(tokens_to_cost(0, 99.0), 0.0);
    }

    #[test]
    fn estimate_run_cost_uses_pricing_currency() {
        let estimate = estimate_run_cost(&billing(), &run("openai", "gpt-test"));
        assert_eq!(estimate.currency, "USD");
    }

    #[test]
    fn estimate_run_cost_partial_input_only_pricing_has_no_total() {
        let mut billing = BillingConfig::default();
        billing.model_pricing.insert(
            "partial".into(),
            ModelPricingConfig {
                provider: "openai".into(),
                model: "gpt-partial".into(),
                input_usd_per_million_tokens: Some(5.0),
                output_usd_per_million_tokens: None,
                currency: "EUR".into(),
            },
        );
        let mut r = run("openai", "gpt-partial");
        r.input_tokens = 1_000_000;
        r.output_tokens = 0;
        let estimate = estimate_run_cost(&billing, &r);
        assert!(estimate.pricing_known);
        assert_eq!(estimate.currency, "EUR");
        assert_eq!(estimate.input_cost_usd, Some(5.0));
        assert_eq!(estimate.output_cost_usd, None);
        assert_eq!(estimate.total_cost_usd, None);
    }

    #[test]
    fn has_admin_role_rejects_empty_roles_map() {
        assert!(!has_admin_role(&admin_claims(HashMap::new())));
    }

    #[test]
    fn has_admin_role_rejects_admin_in_unlisted_product() {
        let mut roles = HashMap::new();
        roles.insert("other".into(), vec![role_entry("admin")]);
        assert!(!has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn has_admin_role_accepts_super_admin_in_admin_product() {
        let mut roles = HashMap::new();
        roles.insert("admin".into(), vec![role_entry("super_admin")]);
        assert!(has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn has_admin_role_rejects_multiple_non_admin_roles() {
        let mut roles = HashMap::new();
        roles.insert(
            "ares".into(),
            vec![role_entry("viewer"), role_entry("editor")],
        );
        assert!(!has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn parse_tenant_tier_accepts_free_and_dev() {
        assert!(matches!(parse_tenant_tier("free").unwrap(), TenantTier::Free));
        assert!(matches!(parse_tenant_tier("DEV").unwrap(), TenantTier::Dev));
    }

    #[test]
    fn provision_client_request_deserializes() {
        let req: ProvisionClientRequest = serde_json::from_str(
            r#"{"name":"Acme","tier":"pro","product_type":"ares","api_key_name":"bootstrap"}"#,
        )
        .unwrap();
        assert_eq!(req.name, "Acme");
        assert_eq!(req.tier, "pro");
        assert_eq!(req.product_type, "ares");
        assert_eq!(req.api_key_name, "bootstrap");
    }

    #[test]
    fn test_tenant_agent_request_deserializes_defaults() {
        let req: TestTenantAgentRequest =
            serde_json::from_str(r#"{"message":"hi","config":{"model":"gpt-4"}}"#).unwrap();
        assert_eq!(req.message, "hi");
        assert!(!req.use_eruka_context);
        assert!(req.workspace_id.is_none());
    }

    #[test]
    fn tenant_response_serializes_fields() {
        let resp = TenantResponse {
            id: "t1".into(),
            name: "Acme".into(),
            tier: "enterprise".into(),
            created_at: 1_700_000_000,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["id"], "t1");
        assert_eq!(json["tier"], "enterprise");
        assert_eq!(json["created_at"], 1_700_000_000);
    }

    #[test]
    fn daily_usage_query_deserializes_optional_days() {
        let q: DailyUsageQuery = serde_json::from_str(r#"{"days":7}"#).unwrap();
        assert_eq!(q.days, Some(7));
        let default: DailyUsageQuery = serde_json::from_str("{}").unwrap();
        assert!(default.days.is_none());
    }

    #[test]
    fn daily_usage_entry_serializes_counters() {
        let entry = DailyUsageEntry {
            date: 1_700_000_000,
            requests: 5,
            tokens: 100,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["requests"], 5);
        assert_eq!(json["tokens"], 100);
    }

    #[test]
    fn agent_runs_query_deserializes_pagination() {
        let q: AgentRunsQuery = serde_json::from_str(r#"{"limit":25,"offset":10}"#).unwrap();
        assert_eq!(q.limit, Some(25));
        assert_eq!(q.offset, Some(10));
    }

    #[test]
    fn agent_feedback_summary_query_deserializes_days() {
        let q: AgentFeedbackSummaryQuery = serde_json::from_str(r#"{"days":14}"#).unwrap();
        assert_eq!(q.days, Some(14));
    }

    #[test]
    fn create_agent_run_feedback_request_deserializes_with_defaults() {
        let req: CreateAgentRunFeedbackRequest =
            serde_json::from_str(r#"{"feedback_type":"quality","score":4.5}"#).unwrap();
        assert_eq!(req.feedback_type, "quality");
        assert_eq!(req.score, Some(4.5));
        assert!(req.flags.is_empty());
        assert!(req.notes.is_none());
    }

    #[test]
    fn alerts_query_deserializes_filters() {
        let q: AlertsQuery =
            serde_json::from_str(r#"{"severity":"critical","resolved":false,"limit":10}"#).unwrap();
        assert_eq!(q.severity.as_deref(), Some("critical"));
        assert_eq!(q.resolved, Some(false));
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn audit_log_query_deserializes_pagination() {
        let q: AuditLogQuery = serde_json::from_str(r#"{"limit":100,"offset":50}"#).unwrap();
        assert_eq!(q.limit, Some(100));
        assert_eq!(q.offset, Some(50));
    }

    #[test]
    fn resolve_alert_request_deserializes_optional_reviewer() {
        let req: ResolveAlertRequest =
            serde_json::from_str(r#"{"resolved_by":"alice"}"#).unwrap();
        assert_eq!(req.resolved_by.as_deref(), Some("alice"));
    }

    #[test]
    fn emergency_stop_request_deserializes_inactive_flag() {
        let req: EmergencyStopRequest = serde_json::from_str(r#"{"active":false}"#).unwrap();
        assert!(!req.active);
    }

    #[test]
    fn cost_estimate_known_serializes_cost_fields() {
        let estimate = estimate_run_cost(&billing(), &run("openai", "gpt-test"));
        let json = serde_json::to_value(&estimate).unwrap();
        assert_eq!(json["pricing_known"], true);
        assert_eq!(json["currency"], "USD");
        assert!(json["total_cost_usd"].is_number());
    }

    #[test]
    fn agent_run_response_serializes_nested_run() {
        let response = AgentRunResponse::from_run(run("openai", "gpt-test"), &billing());
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["id"], "run-1");
        assert_eq!(json["cost_estimate"]["pricing_known"], true);
    }

    #[test]
    fn usage_response_serializes_counters() {
        let resp = UsageResponse::from(UsageSummary {
            monthly_requests: 1,
            monthly_tokens: 2,
            daily_requests: 3,
        });
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["monthly_requests"], 1);
        assert_eq!(json["daily_requests"], 3);
    }

    #[test]
    fn api_key_response_serializes_fields() {
        use crate::models::ApiKey;
        let key = ApiKey::new(
            "k".into(),
            "t".into(),
            "hash".into(),
            "prefix".into(),
            "name".into(),
        );
        let json = serde_json::to_value(ApiKeyResponse::from(key)).unwrap();
        assert_eq!(json["key_prefix"], "prefix");
        assert_eq!(json["is_active"], true);
    }

    #[test]
    fn tenant_response_from_tenant_maps_free_dev_enterprise() {
        use crate::models::Tenant;
        for (tier, expected) in [
            (TenantTier::Free, "free"),
            (TenantTier::Dev, "dev"),
            (TenantTier::Enterprise, "enterprise"),
        ] {
            let tenant = Tenant::new("id".into(), "n".into(), tier);
            assert_eq!(TenantResponse::from(tenant).tier, expected);
        }
    }

    #[test]
    fn test_tenant_agent_response_serializes_status() {
        let resp = TestTenantAgentResponse {
            status: "ok".into(),
            response: Some("hello".into()),
            error: None,
            input_tokens: 1,
            output_tokens: 2,
            duration_ms: 3,
            model_name: Some("gpt".into()),
            provider_name: Some("openai".into()),
            config_source: "tenant_db".into(),
            config_version: "v1".into(),
            workspace_id: None,
            eruka_context_injected: false,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["eruka_context_injected"], false);
    }

    #[test]
    fn provision_client_response_serializes() {
        let resp = ProvisionClientResponse {
            tenant_id: "t".into(),
            tenant_name: "Acme".into(),
            tier: "pro".into(),
            product_type: "ares".into(),
            api_key_id: "key".into(),
            api_key_prefix: "ares_".into(),
            raw_api_key: "secret".into(),
            agents_created: vec!["a1".into()],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["agents_created"][0], "a1");
    }

    #[test]
    fn role_entry_deserializes_resource_id() {
        let entry: RoleEntry =
            serde_json::from_str(r#"{"role":"admin","resource_id":"res-1"}"#).unwrap();
        assert_eq!(entry.role, "admin");
        assert_eq!(entry.resource_id.as_deref(), Some("res-1"));
    }

    #[test]
    fn invalid_tier_message_lists_allowed_values() {
        assert!(INVALID_TIER_MSG.contains("free"));
        assert!(INVALID_TIER_MSG.contains("enterprise"));
    }

    #[test]
    fn app_error_not_found_serializes_for_admin_handlers() {
        let err = AppError::NotFound("Tenant not found".to_string());
        assert!(matches!(err, AppError::NotFound(_)));
        assert!(err.to_string().contains("Tenant not found"));
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
            "All agents are now in emergency stop mode. /api/v1/chat requests will be rejected with 503."
        } else {
            "Emergency stop cleared. Agents are operational."
        }
    })))
}

// =============================================================================
// Fleet Provider Secrets (encrypted at rest, hot-swap in memory)
// =============================================================================

use crate::db::fleet_provider_secrets as fps;
use ares_config::fleet_secrets::{last_n_visible, MasterKey};

/// List every fleet provider override currently stored in the DB.
///
/// Response shape:
/// ```json
/// [
///   {
///     "name": "nvidia",
///     "has_api_key": true,
///     "api_key_last4": "…XYZW",
///     "api_base": "https://integrate.api.nvidia.com/v1",
///     "default_model": null,
///     "updated_at": 1717690000,
///     "updated_by": "admin@x.com"
///   }
/// ]
/// ```
pub async fn list_fleet_providers(
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>> {
    let store = fps::FleetProviderSecretsStore::new(state.tenant_db.pool());
    let master = MasterKey::from_env();
    // Decrypt all rows so the UI can show the last-4 of the stored key.
    // Decryption errors on individual rows are logged and skipped inside
    // `load_all`, so this call never fails because of a single bad row.
    let map = store.load_all(master.as_ref()).await?;

    let mut out: Vec<serde_json::Value> = map
        .into_iter()
        .map(|(name, entry)| {
            let api_key_last4 = entry
                .api_key
                .as_deref()
                .and_then(|k| last_n_visible(k, 4));
            serde_json::json!({
                "name": name,
                "has_api_key": entry.api_key.is_some(),
                "api_key_last4": api_key_last4,
                "api_base": entry.api_base,
                "default_model": entry.default_model,
                "updated_at": entry.updated_at,
                "updated_by": entry.updated_by,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
pub struct FleetProviderUpsertRequest {
    /// Optional. If `None`, the existing key is preserved.
    pub api_key: Option<String>,
    /// Optional. If `None`, the existing api_base is preserved.
    pub api_base: Option<String>,
    /// Optional. If `None`, the existing default_model is preserved.
    pub default_model: Option<String>,
}

/// Upsert a fleet provider override. Encrypts the new key (if any), writes
/// to the DB, and atomically swaps the in-memory map so all readers (the
/// catalog refresh path, the LLM factory) see the new value on their next
/// load.
pub async fn upsert_fleet_provider(
    State(state): State<AppState>,
    Path(provider_name): Path<String>,
    Json(req): Json<FleetProviderUpsertRequest>,
) -> Result<Json<serde_json::Value>> {
    if provider_name.is_empty() {
        return Err(AppError::InvalidInput("provider_name must not be empty".into()));
    }
    if req.api_key.is_none() && req.api_base.is_none() && req.default_model.is_none() {
        return Err(AppError::InvalidInput(
            "At least one of api_key, api_base, default_model must be provided".into(),
        ));
    }

    let store = fps::FleetProviderSecretsStore::new(state.tenant_db.pool());
    let master = MasterKey::from_env();
    if req.api_key.is_some() && master.is_none() {
        return Err(AppError::Configuration(
            "FLEET_SECRETS_KEY is not set; cannot store new API keys. \
             Configure /etc/dirmacs/fleet-secrets.env and reload ares.service."
                .into(),
        ));
    }

    let updated_by = "admin";
    let stored = store
        .upsert(
            &provider_name,
            req.api_key.as_deref(),
            req.api_base.as_deref(),
            req.default_model.as_deref(),
            master.as_ref(),
            updated_by,
        )
        .await?;

    // Reload + atomically swap the in-memory map. The store gives us the
    // encrypted form; we need the decrypted form for the in-memory cache.
    let map = store.load_all(master.as_ref()).await?;
    state.fleet_secrets.store(map);

    // Audit log — redact the raw key, only emit the boolean + last-4.
    let details = serde_json::json!({
        "api_key_set": stored.has_api_key,
        "api_key_last4": req.api_key.as_deref().and_then(|k| last_n_visible(k, 4)),
        "api_base_set": stored.api_base.is_some(),
        "default_model_set": stored.default_model.is_some(),
    })
    .to_string();
    let pool = state.tenant_db.pool().clone();
    let name = provider_name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "fleet_provider_upsert",
            "fleet_provider",
            &name,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "name": provider_name,
        "has_api_key": stored.has_api_key,
        "api_base": stored.api_base,
        "default_model": stored.default_model,
        "updated_at": stored.updated_at,
        "updated_by": stored.updated_by,
    })))
}

/// Hard-delete a fleet provider override. Gone from the DB and from the
/// in-memory cache. Re-add via the UI to bring it back.
pub async fn delete_fleet_provider(
    State(state): State<AppState>,
    Path(provider_name): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let store = fps::FleetProviderSecretsStore::new(state.tenant_db.pool());
    let affected = store.delete(&provider_name).await?;
    if affected == 0 {
        return Err(AppError::NotFound(format!(
            "Fleet provider '{}' not found",
            provider_name
        )));
    }

    // Reload + atomically swap the in-memory map.
    let master = MasterKey::from_env();
    let map = store.load_all(master.as_ref()).await?;
    state.fleet_secrets.store(map);

    let pool = state.tenant_db.pool().clone();
    let name = provider_name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "fleet_provider_delete",
            "fleet_provider",
            &name,
            None,
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "name": provider_name,
        "deleted": true
    })))
}

#[derive(Debug, Serialize)]
pub struct FleetProviderVerifyResponse {
    pub name: String,
    pub ok: bool,
    pub latency_ms: u64,
    pub model_count: usize,
    pub models: Vec<String>,
    pub error: Option<String>,
}

/// Verify a stored provider's API key by calling its model listing endpoint.
/// For OpenAI-compatible providers we call `<api_base>/models` with the
/// stored key. For Anthropic/Ollama (when those features are enabled) the
/// call shape is different — the UI surfaces a clear error in that case
/// and tells the operator to test via the provider's own dashboard.
pub async fn verify_fleet_provider(
    State(state): State<AppState>,
    Path(provider_name): Path<String>,
) -> Result<Json<FleetProviderVerifyResponse>> {
    let start = std::time::Instant::now();

    // Look up the in-memory override (decrypted).
    let entry = state.fleet_secrets.get(&provider_name);
    let override_ = entry.clone();

    // Fall back to the registry's ProviderConfig for the base URL when the
    // admin hasn't set an override.
    let provider_config = state.provider_registry.get_provider(&provider_name).cloned();

    let (api_base, api_key) = match (override_, provider_config.as_ref()) {
        (Some(o), _) => {
            let key = o.api_key.clone();
            let base = o
                .api_base
                .clone()
                .or_else(|| provider_config.as_ref().and_then(|pc| default_api_base(pc)));
            (base, key)
        }
        (None, Some(pc)) => (default_api_base(pc), resolve_env_key(pc)),
        (None, None) => (None, None),
    };

    let api_key = match api_key {
        Some(k) if !k.is_empty() => k,
        _ => {
            return Ok(Json(FleetProviderVerifyResponse {
                name: provider_name,
                ok: false,
                latency_ms: start.elapsed().as_millis() as u64,
                model_count: 0,
                models: vec![],
                error: Some(
                    "No API key configured for this provider. Set one in the Fleet Providers card."
                        .into(),
                ),
            }));
        }
    };

    let api_base = match api_base {
        Some(b) if !b.is_empty() => b,
        _ => {
            return Ok(Json(FleetProviderVerifyResponse {
                name: provider_name,
                ok: false,
                latency_ms: start.elapsed().as_millis() as u64,
                model_count: 0,
                models: vec![],
                error: Some("No API base URL configured for this provider.".into()),
            }));
        }
    };

    let models_url = format!("{}/models", api_base.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = match client
        .get(&models_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return Ok(Json(FleetProviderVerifyResponse {
                name: provider_name,
                ok: false,
                latency_ms: start.elapsed().as_millis() as u64,
                model_count: 0,
                models: vec![],
                error: Some(format!("HTTP request failed: {e}")),
            }));
        }
    };

    let status = resp.status();
    let latency_ms = start.elapsed().as_millis() as u64;
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Ok(Json(FleetProviderVerifyResponse {
            name: provider_name,
            ok: false,
            latency_ms,
            model_count: 0,
            models: vec![],
            error: Some(format!("HTTP {} — {}", status.as_u16(), body)),
        }));
    }

    let parsed: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return Ok(Json(FleetProviderVerifyResponse {
                name: provider_name,
                ok: false,
                latency_ms,
                model_count: 0,
                models: vec![],
                error: Some(format!("JSON parse failed: {e}")),
            }));
        }
    };

    // OpenAI-compatible providers return `{"data": [{"id": "..."}]}`.
    // Pull ids out of `data[*].id`. If `data` is missing, return empty list.
    let models: Vec<String> = parsed
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("id").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let model_count = models.len();

    Ok(Json(FleetProviderVerifyResponse {
        name: provider_name,
        ok: true,
        latency_ms,
        model_count,
        models,
        error: None,
    }))
}

#[derive(Debug, Serialize)]
pub struct FleetProviderCapabilities {
    /// Provider type identifiers supported by this build.
    pub providers: Vec<&'static str>,
    /// Whether fleet-secrets encryption is enabled (FLEET_SECRETS_KEY set).
    pub encryption_enabled: bool,
}

/// Return the list of supported provider types based on compiled-in
/// features. The admin UI uses this to filter the type dropdown so users
/// can only select providers that this build can actually instantiate.
pub async fn fleet_provider_capabilities() -> Json<FleetProviderCapabilities> {
    let providers: Vec<&'static str> = vec!["openai"];
    #[cfg(feature = "anthropic")]
    providers.push("anthropic");
    #[cfg(feature = "ollama")]
    providers.push("ollama");

    Json(FleetProviderCapabilities {
        providers,
        encryption_enabled: MasterKey::from_env().is_some(),
    })
}

fn default_api_base(pc: &ProviderConfig) -> Option<String> {
    match pc {
        ProviderConfig::OpenAI { api_base, .. } => Some(api_base.clone()),
        ProviderConfig::Anthropic { .. } => Some("https://api.anthropic.com".to_string()),
        ProviderConfig::Ollama { base_url, .. } => Some(base_url.clone()),
        _ => None,
    }
}

fn resolve_env_key(pc: &ProviderConfig) -> Option<String> {
    let var = match pc {
        ProviderConfig::OpenAI { api_key_env, .. } => api_key_env,
        ProviderConfig::Anthropic { api_key_env, .. } => api_key_env,
        ProviderConfig::Ollama { api_key_env, .. } => api_key_env,
        _ => return None,
    };
    std::env::var(var).ok()
}

// =============================================================================
// Runtime Tools (CRUD + versions + rollback + test)
// =============================================================================

use ares_db::runtime_tools::{
    CreateRuntimeToolRequest, RuntimeToolStore, UpdateRuntimeToolRequest,
};

/// List every runtime tool currently stored in the DB.
pub async fn list_runtime_tools(
    State(state): State<AppState>,
) -> Result<Json<Vec<ares_db::runtime_tools::RuntimeTool>>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let tools = store.get_all().await?;
    Ok(Json(tools))
}

/// Get a single runtime tool by its UUID.
pub async fn get_runtime_tool(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ares_db::runtime_tools::RuntimeTool>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let tool = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("runtime tool {id} not found")))?;
    Ok(Json(tool))
}

/// Create a new runtime tool. After a successful DB insert the in-memory
/// [`RuntimeToolRegistry`] is reloaded so agents see the tool immediately.
pub async fn create_runtime_tool(
    State(state): State<AppState>,
    Json(req): Json<CreateRuntimeToolRequest>,
) -> Result<Json<serde_json::Value>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let tool = store.create(&req).await?;

    if let Err(e) = state.runtime_tool_registry.reload().await {
        tracing::warn!("Failed to hot-reload runtime tools after create: {}", e);
    }

    let pool = state.tenant_db.pool().clone();
    let tool_id = tool.id.clone();
    let tool_name = tool.name.clone();
    let tool_type = tool.tool_type.clone();
    tokio::spawn(async move {
        let details = serde_json::json!({
            "name": tool_name,
            "tool_type": tool_type,
            "enabled": tool.enabled,
        })
        .to_string();
        let _ = audit_log::log_admin_action(
            &pool,
            "runtime_tool_create",
            "runtime_tool",
            &tool_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "id": tool.id,
        "name": tool.name,
        "version": tool.version,
        "created_at": tool.created_at,
    })))
}

/// Update a runtime tool. The DB row is patched and the change is automatically
/// snapshotted into `runtime_tool_versions` before the registry is reloaded.
pub async fn update_runtime_tool(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateRuntimeToolRequest>,
) -> Result<Json<serde_json::Value>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let tool = store.update(&id, &req).await?;

    if let Err(e) = state.runtime_tool_registry.reload().await {
        tracing::warn!("Failed to hot-reload runtime tools after update: {}", e);
    }

    let pool = state.tenant_db.pool().clone();
    let tool_id = id.clone();
    let new_version = tool.version;
    tokio::spawn(async move {
        let details = serde_json::json!({ "version": new_version }).to_string();
        let _ = audit_log::log_admin_action(
            &pool,
            "runtime_tool_update",
            "runtime_tool",
            &tool_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "id": tool.id,
        "name": tool.name,
        "version": tool.version,
        "updated_at": tool.updated_at,
    })))
}

/// Hard-delete a runtime tool (cascades to versions & executions).
pub async fn delete_runtime_tool(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let affected = store.delete(&id).await?;
    if affected == 0 {
        return Err(AppError::NotFound(format!("runtime tool {id} not found")));
    }

    if let Err(e) = state.runtime_tool_registry.reload().await {
        tracing::warn!("Failed to hot-reload runtime tools after delete: {}", e);
    }

    let pool = state.tenant_db.pool().clone();
    let tool_id = id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "runtime_tool_delete",
            "runtime_tool",
            &tool_id,
            None,
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "id": id,
        "deleted": true,
    })))
}

#[derive(Debug, Deserialize)]
pub struct TestRuntimeToolRequest {
    /// JSON arguments passed to the tool's `execute` method.
    pub input_args: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct TestRuntimeToolResponse {
    pub ok: bool,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub latency_ms: u64,
}

/// Execute a runtime tool with sample input and record the result.
pub async fn test_runtime_tool(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TestRuntimeToolRequest>,
) -> Result<Json<TestRuntimeToolResponse>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    let tool = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("runtime tool {id} not found")))?;

    let start = std::time::Instant::now();

    // If the registry doesn't yet contain this tool (e.g. first test after
    // creation), force a reload before executing.
    if state.runtime_tool_registry.get(&tool.name).is_none() {
        if let Err(e) = state.runtime_tool_registry.reload().await {
            tracing::warn!("Failed to reload runtime tools before test: {}", e);
        }
    }

    let result = state
        .runtime_tool_registry
        .execute(&tool.name, req.input_args.clone())
        .await;
    let latency_ms = start.elapsed().as_millis() as u64;

    let (ok, output, error) = match result {
        Ok(v) => (true, Some(v), None),
        Err(e) => (false, None, Some(e.to_string())),
    };

    let log_result = store
        .log_execution(
            &id,
            None, // tenant_id
            None, // agent_run_id
            &req.input_args,
            output.as_ref(),
            if ok { "success" } else { "error" },
            error.as_deref(),
            latency_ms as i64,
        )
        .await;

    if let Err(e) = log_result {
        tracing::warn!("Failed to log runtime tool test execution: {}", e);
    }

    let pool = state.tenant_db.pool().clone();
    let tool_id = id.clone();
    tokio::spawn(async move {
        let details = serde_json::json!({
            "ok": ok,
            "latency_ms": latency_ms,
        })
        .to_string();
        let _ = audit_log::log_admin_action(
            &pool,
            "runtime_tool_test",
            "runtime_tool",
            &tool_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(TestRuntimeToolResponse {
        ok,
        output,
        error,
        latency_ms,
    }))
}

/// Return the version history for a runtime tool.
pub async fn list_runtime_tool_versions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ares_db::runtime_tools::RuntimeToolVersion>>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());
    // Verify the tool exists before returning versions.
    let _ = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("runtime tool {id} not found")))?;

    let versions = store.get_versions(&id, 100).await?;
    Ok(Json(versions))
}

/// Rollback a runtime tool to a previous version.
///
/// The target version's `parameters_schema` and `execution_config` (and
/// `description`) are applied via the normal `update` path, which
/// automatically snapshots the current state as a new version entry.
pub async fn rollback_runtime_tool(
    State(state): State<AppState>,
    Path((id, version)): Path<(String, i32)>,
) -> Result<Json<serde_json::Value>> {
    let store = RuntimeToolStore::new(state.tenant_db.pool());

    let _ = store
        .get_by_id(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("runtime tool {id} not found")))?;

    let versions = store.get_versions(&id, 1000).await?;
    let target = versions
        .into_iter()
        .find(|v| v.version == version)
        .ok_or_else(|| {
            AppError::NotFound(format!("version {version} not found for tool {id}"))
        })?;

    let update_req = UpdateRuntimeToolRequest {
        display_name: None,
        description: target.description,
        parameters_schema: Some(target.parameters_schema),
        execution_config: Some(target.execution_config),
        enabled: None,
        is_public: None,
    };

    let updated = store.update(&id, &update_req).await?;

    if let Err(e) = state.runtime_tool_registry.reload().await {
        tracing::warn!("Failed to hot-reload runtime tools after rollback: {}", e);
    }

    let pool = state.tenant_db.pool().clone();
    let tool_id = id.clone();
    let new_version = updated.version;
    tokio::spawn(async move {
        let details = serde_json::json!({
            "rolled_back_to": version,
            "new_version": new_version,
        })
        .to_string();
        let _ = audit_log::log_admin_action(
            &pool,
            "runtime_tool_rollback",
            "runtime_tool",
            &tool_id,
            Some(&details),
            None,
        )
        .await;
    });

    Ok(Json(serde_json::json!({
        "id": updated.id,
        "name": updated.name,
        "version": updated.version,
        "rolled_back_to": version,
        "updated_at": updated.updated_at,
    })))
}

// =============================================================================
// Run History
// =============================================================================

use crate::db::run_history::{
    AcknowledgeBudgetAlertRequest, AgentHealthMetrics, BudgetAlert, ListBudgetAlertsQuery,
    ListLlmCallsQuery, ListToolCallsQuery, LogLlmCallRequest, LogToolCallRequest, RunCost,
    RunHistoryStore, RunLlmCall, RunToolCall, SetTenantBudgetRequest, TenantBudget,
};

#[derive(Debug, Deserialize)]
pub struct ListRunCostsQuery {
    pub tenant_id: String,
    #[serde(default = "default_list_limit")]
    pub limit: i32,
    #[serde(default)]
    pub offset: i32,
}

#[derive(Debug, Deserialize)]
pub struct ListHealthMetricsQuery {
    pub tenant_id: String,
    #[serde(default = "default_list_limit")]
    pub limit: i32,
    #[serde(default)]
    pub offset: i32,
}

fn default_list_limit() -> i32 {
    50
}

/// List LLM calls with optional filtering.
pub async fn list_llm_calls(
    State(state): State<AppState>,
    Query(q): Query<ListLlmCallsQuery>,
) -> Result<Json<Vec<RunLlmCall>>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let calls = store.list_llm_calls(&q).await?;
    Ok(Json(calls))
}

/// Get a single LLM call by id.
pub async fn get_llm_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunLlmCall>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let call = store
        .get_llm_call(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("llm call {id} not found")))?;
    Ok(Json(call))
}

/// Insert a new LLM call record.
pub async fn insert_llm_call(
    State(state): State<AppState>,
    Json(req): Json<LogLlmCallRequest>,
) -> Result<Json<RunLlmCall>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let call = store.insert_llm_call(&req).await?;
    Ok(Json(call))
}

/// List tool calls with optional filtering.
pub async fn list_tool_calls(
    State(state): State<AppState>,
    Query(q): Query<ListToolCallsQuery>,
) -> Result<Json<Vec<RunToolCall>>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let calls = store.list_tool_calls(&q).await?;
    Ok(Json(calls))
}

/// Get a single tool call by id.
pub async fn get_tool_call(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunToolCall>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let call = store
        .get_tool_call(&id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("tool call {id} not found")))?;
    Ok(Json(call))
}

/// Insert a new tool call record.
pub async fn insert_tool_call(
    State(state): State<AppState>,
    Json(req): Json<LogToolCallRequest>,
) -> Result<Json<RunToolCall>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let call = store.insert_tool_call(&req).await?;
    Ok(Json(call))
}

/// Get a run cost by run_id.
pub async fn get_run_cost(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<RunCost>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let cost = store
        .get_run_cost(&run_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("run cost {run_id} not found")))?;
    Ok(Json(cost))
}

/// List run costs for a tenant.
pub async fn list_run_costs(
    State(state): State<AppState>,
    Query(q): Query<ListRunCostsQuery>,
) -> Result<Json<Vec<RunCost>>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let costs = store.list_run_costs(&q.tenant_id, q.limit, q.offset).await?;
    Ok(Json(costs))
}

/// Get a tenant budget.
pub async fn get_tenant_budget(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<TenantBudget>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let budget = store
        .get_tenant_budget(&tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("tenant budget {tenant_id} not found")))?;
    Ok(Json(budget))
}

/// Set (upsert) a tenant budget.
pub async fn set_tenant_budget(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Json(mut req): Json<SetTenantBudgetRequest>,
) -> Result<Json<TenantBudget>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    req.tenant_id = tenant_id;
    let budget = store.set_tenant_budget(&req).await?;
    Ok(Json(budget))
}

/// Delete a tenant budget.
pub async fn delete_tenant_budget(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let rows = store.delete_tenant_budget(&tenant_id).await?;
    Ok(Json(serde_json::json!({ "deleted": rows > 0, "tenant_id": tenant_id })))
}

/// List budget alerts with optional filtering.
pub async fn list_budget_alerts(
    State(state): State<AppState>,
    Query(q): Query<ListBudgetAlertsQuery>,
) -> Result<Json<Vec<BudgetAlert>>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let alerts = store.list_budget_alerts(&q).await?;
    Ok(Json(alerts))
}

/// Acknowledge a budget alert.
pub async fn acknowledge_budget_alert(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AcknowledgeBudgetAlertRequest>,
) -> Result<Json<BudgetAlert>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let alert = store.acknowledge_budget_alert(&id, &req).await?;
    Ok(Json(alert))
}

/// List health metrics for a tenant.
pub async fn list_health_metrics(
    State(state): State<AppState>,
    Query(q): Query<ListHealthMetricsQuery>,
) -> Result<Json<Vec<AgentHealthMetrics>>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let metrics = store.list_health_metrics(&q.tenant_id, q.limit, q.offset).await?;
    Ok(Json(metrics))
}

/// Insert agent health metrics.
pub async fn insert_health_metrics(
    State(state): State<AppState>,
    Json(req): Json<AgentHealthMetrics>,
) -> Result<Json<AgentHealthMetrics>> {
    let store = RunHistoryStore::new(state.tenant_db.pool());
    let metrics = store.insert_health_metrics(&req).await?;
    Ok(Json(metrics))
}

// =============================================================================
// Tenant Model Tiers (per-tenant abstract tier -> concrete provider/model)
// =============================================================================

pub async fn list_tenant_model_tiers(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<db_tiers::TenantModelTier>>> {
    let store = db_tiers::TenantModelTierStore::new(state.tenant_db.pool());
    let tiers = store.list_for_tenant(&tenant_id).await?;
    Ok(Json(tiers))
}

pub async fn get_tenant_model_tier(
    State(state): State<AppState>,
    Path((tenant_id, tier_name)): Path<(String, String)>,
) -> Result<Json<db_tiers::TenantModelTier>> {
    let store = db_tiers::TenantModelTierStore::new(state.tenant_db.pool());
    let tier = store
        .get(&tenant_id, &tier_name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("tier {tier_name} not found for tenant {tenant_id}")))?;
    Ok(Json(tier))
}

pub async fn set_tenant_model_tier(
    State(state): State<AppState>,
    Path((tenant_id, tier_name)): Path<(String, String)>,
    Json(req): Json<db_tiers::SetTenantModelTierRequest>,
) -> Result<Json<db_tiers::TenantModelTier>> {
    let store = db_tiers::TenantModelTierStore::new(state.tenant_db.pool());
    let tier = store.set(&tenant_id, &tier_name, &req).await?;

    let pool = state.tenant_db.pool().clone();
    let t_id = tenant_id.clone();
    let t_name = tier_name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "tenant_model_tier_set",
            "tenant_model_tier",
            &format!("{t_id}/{t_name}"),
            None,
            None,
        )
        .await;
    });

    Ok(Json(tier))
}

pub async fn delete_tenant_model_tier(
    State(state): State<AppState>,
    Path((tenant_id, tier_name)): Path<(String, String)>,
) -> Result<StatusCode> {
    let store = db_tiers::TenantModelTierStore::new(state.tenant_db.pool());
    let rows = store.delete(&tenant_id, &tier_name).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "tier {tier_name} not found for tenant {tenant_id}"
        )));
    }

    let pool = state.tenant_db.pool().clone();
    let t_id = tenant_id.clone();
    let t_name = tier_name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "tenant_model_tier_delete",
            "tenant_model_tier",
            &format!("{t_id}/{t_name}"),
            None,
            None,
        )
        .await;
    });

    Ok(StatusCode::NO_CONTENT)
}
