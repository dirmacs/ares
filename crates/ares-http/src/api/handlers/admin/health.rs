//! Admin health domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use super::*;
use ::cordis::Context;
use std::sync::Arc;

use crate::HttpError;
use crate::Result;
use ares_store::audit_log;
use ares_store::tenant_model_tiers as db_tiers;
use ares_types::types::AppError;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use sha2::Digest;

pub async fn list_health_metrics(
    State(ctx): State<Arc<Context>>,
    Query(q): Query<ListHealthMetricsQuery>,
) -> Result<Json<Vec<AgentHealthMetrics>>> {
    let __pool_1 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
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
    let __pool_2 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
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
    let __pool_3 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let store = RunHistoryStore::new(&__pool_3);
    let metrics = store.insert_health_metrics(&req).await?;
    Ok(Json(metrics))
}

pub async fn list_tenant_model_tiers(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<db_tiers::TenantModelTier>>> {
    let __pool_4 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let store = db_tiers::TenantModelTierStore::new(&__pool_4);
    let tiers = store.list_for_tenant(&tenant_id).await?;
    Ok(Json(tiers))
}

pub async fn get_tenant_model_tier(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, tier_name)): Path<(String, String)>,
) -> Result<Json<db_tiers::TenantModelTier>> {
    let __pool_5 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let store = db_tiers::TenantModelTierStore::new(&__pool_5);
    let tier = store.get(&tenant_id, &tier_name).await?.ok_or_else(|| {
        AppError::NotFound(format!("tier {tier_name} not found for tenant {tenant_id}"))
    })?;
    Ok(Json(tier))
}

pub async fn set_tenant_model_tier(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, tier_name)): Path<(String, String)>,
    actor: AdminActor,
    Json(req): Json<db_tiers::SetTenantModelTierRequest>,
) -> Result<Json<db_tiers::TenantModelTier>> {
    if !ctx
        .get::<ares_llm::Llm>()
        .expect("not provided")
        .has_provider_for_tenant(&req.provider_name, Some(&tenant_id))
    {
        return Err(HttpError::from(AppError::InvalidInput(format!(
            "Provider '{}' not found in configuration",
            req.provider_name
        ))));
    }

    let __pool_6 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let store = db_tiers::TenantModelTierStore::new(&__pool_6);
    let tier = store.set(&tenant_id, &tier_name, &req).await?;

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let t_id = tenant_id.clone();
    let t_name = tier_name.clone();
    audit_log::record(
        &pool,
        "tenant_model_tier_set",
        "tenant_model_tier",
        &format!("{t_id}/{t_name}"),
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(Json(tier))
}

pub async fn delete_tenant_model_tier(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, tier_name)): Path<(String, String)>,
    actor: AdminActor,
) -> Result<StatusCode> {
    let __pool_7 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let store = db_tiers::TenantModelTierStore::new(&__pool_7);
    let rows = store.delete(&tenant_id, &tier_name).await?;
    if rows == 0 {
        return Err(HttpError::from(AppError::NotFound(format!(
            "tier {tier_name} not found for tenant {tenant_id}"
        ))));
    }

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let t_id = tenant_id.clone();
    let t_name = tier_name.clone();
    audit_log::record(
        &pool,
        "tenant_model_tier_delete",
        "tenant_model_tier",
        &format!("{t_id}/{t_name}"),
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

use ::cordis::Service;
