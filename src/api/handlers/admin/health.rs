//! Admin health domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use std::sync::Arc;
use ares_cordis_core::Context;
use super::*;


use crate::AppState;
use crate::db::audit_log;
use crate::db::tenant_model_tiers as db_tiers;
use crate::types::{AppError, Result};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use sha2::Digest;

pub async fn list_health_metrics(
    State(ctx): State<Arc<Context>>,
    Query(q): Query<ListHealthMetricsQuery>,
) -> Result<Json<Vec<AgentHealthMetrics>>> {
    let __pool_1 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_1);
    let metrics = store
        .list_health_metrics(&q.tenant_id, q.limit, q.offset)
        .await?;
    Ok(Json(metrics))
}

/// List model health metrics for a tenant, grouped by (tenant_id, model).
pub async fn list_model_metrics(
    State(ctx): State<Arc<Context>>,
    Query(q): Query<ListModelMetricsQuery>,
) -> Result<Json<Vec<ModelHealthMetrics>>> {
    let __pool_2 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_2);
    let metrics = store
        .list_model_metrics(&q.tenant_id, q.limit, q.offset)
        .await?;
    Ok(Json(metrics))
}

/// Insert agent health metrics.
pub async fn insert_health_metrics(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<AgentHealthMetrics>,
) -> Result<Json<AgentHealthMetrics>> {
    let __pool_3 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = RunHistoryStore::new(&__pool_3);
    let metrics = store.insert_health_metrics(&req).await?;
    Ok(Json(metrics))
}

pub async fn list_tenant_model_tiers(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<db_tiers::TenantModelTier>>> {
    let __pool_4 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_tiers::TenantModelTierStore::new(&__pool_4);
    let tiers = store.list_for_tenant(&tenant_id).await?;
    Ok(Json(tiers))
}

pub async fn get_tenant_model_tier(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, tier_name)): Path<(String, String)>,
) -> Result<Json<db_tiers::TenantModelTier>> {
    let __pool_5 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_tiers::TenantModelTierStore::new(&__pool_5);
    let tier = store.get(&tenant_id, &tier_name).await?.ok_or_else(|| {
        AppError::NotFound(format!("tier {tier_name} not found for tenant {tenant_id}"))
    })?;
    Ok(Json(tier))
}

pub async fn set_tenant_model_tier(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, tier_name)): Path<(String, String)>,
    Json(req): Json<db_tiers::SetTenantModelTierRequest>,
) -> Result<Json<db_tiers::TenantModelTier>> {
    if !ctx.get::<crate::ProviderRegistry>().expect("not provided")
        .has_provider_for_tenant(&req.provider_name, Some(&tenant_id))
    {
        return Err(AppError::InvalidInput(format!(
            "Provider '{}' not found in configuration",
            req.provider_name
        )));
    }

    let __pool_6 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_tiers::TenantModelTierStore::new(&__pool_6);
    let tier = store.set(&tenant_id, &tier_name, &req).await?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
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
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, tier_name)): Path<(String, String)>,
) -> Result<StatusCode> {
    let __pool_7 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_tiers::TenantModelTierStore::new(&__pool_7);
    let rows = store.delete(&tenant_id, &tier_name).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "tier {tier_name} not found for tenant {tenant_id}"
        )));
    }

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
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

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/health/list_health_metrics", get(list_health_metrics))
        .route("/health/list_model_metrics", get(list_model_metrics))
        .route("/health/insert_health_metrics", post(insert_health_metrics))
        .route("/health/list_tenant_model_tiers", get(list_tenant_model_tiers))
        .route("/health/get_tenant_model_tier", get(get_tenant_model_tier))
        .route("/health/set_tenant_model_tier", put(set_tenant_model_tier))
        .route("/health/delete_tenant_model_tier", delete(delete_tenant_model_tier))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ares_cordis_core::Service;
pub struct AdminHealthService;
impl Service for AdminHealthService {}
