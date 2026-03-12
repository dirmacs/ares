use crate::db::tenants::UsageSummary;
use crate::db::tenant_agents::{
    AgentTemplate, CreateTenantAgentRequest, TenantAgent, UpdateTenantAgentRequest,
    clone_templates_for_tenant, create_tenant_agent as db_create_tenant_agent,
    delete_tenant_agent as db_delete_tenant_agent, list_agent_templates,
    list_tenant_agents as db_list_tenant_agents,
    update_tenant_agent as db_update_tenant_agent,
};
use crate::db::product_schema::{ensure_product_schema, ProductType};
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
    let product_type = ProductType::from_str(&req.product_type).ok_or_else(|| {
        AppError::InvalidInput(format!("Invalid product_type '{}'. Must be: generic, kasino, ehb", req.product_type))
    })?;

    let tier = TenantTier::from_str(&req.tier).ok_or_else(|| {
        AppError::InvalidInput("Invalid tier. Must be: free, dev, pro, or enterprise".to_string())
    })?;

    let tenant = state.tenant_db.create_tenant(req.name, tier).await?;

    ensure_product_schema(state.tenant_db.pool(), &product_type).await?;

    let agents = clone_templates_for_tenant(
        state.tenant_db.pool(),
        &tenant.id,
        product_type.as_str(),
    ).await?;

    let (api_key, raw_key) = state.tenant_db.create_api_key(&tenant.id, req.api_key_name).await?;

    Ok(Json(ProvisionClientResponse {
        tenant_id: tenant.id,
        tenant_name: tenant.name,
        tier: tenant.tier.as_str().to_string(),
        product_type: product_type.as_str().to_string(),
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
    Ok(Json(agent))
}

pub async fn update_tenant_agent_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    Json(req): Json<UpdateTenantAgentRequest>,
) -> Result<Json<TenantAgent>> {
    let agent = db_update_tenant_agent(state.tenant_db.pool(), &tenant_id, &agent_name, req).await?;
    Ok(Json(agent))
}

pub async fn delete_tenant_agent_handler(
    State(state): State<AppState>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<StatusCode> {
    db_delete_tenant_agent(state.tenant_db.pool(), &tenant_id, &agent_name).await?;
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
