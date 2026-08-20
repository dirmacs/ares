//! Admin tenants domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use super::*;


use crate::AppState;
use crate::agents::context_provider::AgentRuntimeContext;
use crate::agents::tenant_agent;
use crate::db::agent_feedback;
use crate::db::agent_runs;
use crate::db::agent_versions;
use crate::db::alerts as db_alerts;
use crate::db::audit_log;
use crate::db::schedules as db_schedules;
use crate::db::skills as db_skills;
use crate::db::tenant_agents::{
    AgentTemplate, AgentTemplateStore, CreateTemplateRequest, CreateTenantAgentRequest,
    TenantAgent, UpdateTenantAgentRequest, clone_templates_for_tenant,
    create_tenant_agent as db_create_tenant_agent, delete_tenant_agent as db_delete_tenant_agent,
    get_tenant_agent as db_get_tenant_agent, list_agent_templates, list_tenant_agent_versions,
    list_tenant_agents as db_list_tenant_agents, record_tenant_agent_version,
    rollback_tenant_agent_version, update_tenant_agent as db_update_tenant_agent,
};
use crate::db::tenant_allowlist as allowlist;
use crate::db::tenant_model_tiers as db_tiers;
use crate::db::tenants::UsageSummary;
use crate::llm::provider_registry::{ModelInfo, RuntimeProviderEntry};
use crate::memory::estimate_tokens;
use crate::models::{Tenant, TenantTier};
use crate::types::{AgentContext, AppError, Result};
use crate::utils::toml_config::BillingConfig;
use ares_config::toml_config::ProviderConfig;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{Redirect, Response},
};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

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

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post, put};
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

// TODO: ctx.plugin(AdminTenantsRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminTenantsService;
// impl Service for AdminTenantsService {
//     fn name(&self) -> &'static str { "admin_tenants" }
//     fn check(&self) -> bool { true }
// }