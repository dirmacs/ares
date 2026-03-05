use crate::db::tenants::UsageSummary;
use crate::models::{Tenant, TenantTier};
use crate::types::{AppError, Result};
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};

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
