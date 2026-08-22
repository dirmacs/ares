//! Admin tenants domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use std::sync::Arc;
use ::cordis::Context;
use super::*;


use crate::AppState;
use crate::db::audit_log;
use crate::db::tenant_agents::clone_templates_for_tenant;
use crate::types::{AppError, Result};
use axum::{
    Json,
    extract::{Path, State},
};
use sha2::Digest;

pub async fn create_tenant(
    State(ctx): State<Arc<Context>>,
    Json(payload): Json<CreateTenantRequest>,
) -> Result<Json<TenantResponse>> {
    let tier = parse_tenant_tier(&payload.tier)?;

    let tenant = ctx.get::<crate::TenantDb>().expect("not provided").create_tenant(payload.name, tier).await?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let tid = tenant.id.clone();
    tokio::spawn(async move {
        let _ =
            audit_log::log_admin_action(&pool, "create_tenant", "tenant", &tid, None, None).await;
    });

    Ok(Json(TenantResponse::from(tenant)))
}

pub async fn list_tenants(State(ctx): State<Arc<Context>>) -> Result<Json<Vec<TenantResponse>>> {
    let tenants = ctx.get::<crate::TenantDb>().expect("not provided").list_tenants().await?;
    let response: Vec<TenantResponse> = tenants.into_iter().map(|t| t.into()).collect();

    Ok(Json(response))
}

pub async fn get_tenant(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<TenantResponse>> {
    let tenant = ctx.get::<crate::TenantDb>().expect("not provided")
        .get_tenant(&tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Tenant not found".to_string()))?;

    Ok(Json(TenantResponse::from(tenant)))
}

pub async fn create_api_key(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<Json<serde_json::Value>> {
    let (api_key, raw_key) = ctx.get::<crate::TenantDb>().expect("not provided")
        .create_api_key(&tenant_id, payload.name)
        .await?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
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
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<ApiKeyResponse>>> {
    let keys = ctx.get::<crate::TenantDb>().expect("not provided").list_api_keys(&tenant_id).await?;
    let response: Vec<ApiKeyResponse> = keys.into_iter().map(|k| k.into()).collect();

    Ok(Json(response))
}

pub async fn get_tenant_usage(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<UsageResponse>> {
    let _ = ctx.get::<crate::TenantDb>().expect("not provided")
        .get_tenant(&tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Tenant not found".to_string()))?;

    let usage = ctx.get::<crate::TenantDb>().expect("not provided").get_usage_summary(&tenant_id).await?;

    Ok(Json(UsageResponse::from(usage)))
}

pub async fn update_tenant_quota(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Json(payload): Json<UpdateQuotaRequest>,
) -> Result<Json<TenantResponse>> {
    let tier = parse_tenant_tier(&payload.tier)?;

    ctx.get::<crate::TenantDb>().expect("not provided")
        .update_tenant_quota(&tenant_id, tier)
        .await?;

    let tenant = ctx.get::<crate::TenantDb>().expect("not provided")
        .get_tenant(&tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Tenant not found".to_string()))?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
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

pub async fn provision_client(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<ProvisionClientRequest>,
) -> Result<Json<ProvisionClientResponse>> {
    let tier = parse_tenant_tier(&req.tier)?;

    // product_type is used only to select which agent templates to clone into tenant_agents.
    // It does NOT create product-specific DB tables — client domain data lives in the client's own backend.
    let product_type = req.product_type.to_lowercase();

    let tenant = ctx.get::<crate::TenantDb>().expect("not provided").create_tenant(req.name, tier).await?;

    let agents =
        clone_templates_for_tenant(&ctx.get::<crate::TenantDb>().expect("not provided").pool().clone(), &tenant.id, &product_type).await?;

    let (api_key, raw_key) = ctx.get::<crate::TenantDb>().expect("not provided")
        .create_api_key(&tenant.id, req.api_key_name)
        .await?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
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

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{get, post, put};
    axum::Router::new()
        .route("/tenants/create_tenant", post(create_tenant))
        .route("/tenants/list_tenants", get(list_tenants))
        .route("/tenants/get_tenant", get(get_tenant))
        .route("/tenants/create_api_key", post(create_api_key))
        .route("/tenants/list_api_keys", get(list_api_keys))
        .route("/tenants/get_tenant_usage", get(get_tenant_usage))
        .route("/tenants/update_tenant_quota", put(update_tenant_quota))
        .route("/tenants/provision_client", post(provision_client))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ::cordis::Service;
pub struct AdminTenantsService;
impl Service for AdminTenantsService {}