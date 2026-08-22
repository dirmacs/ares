//! Admin pipelines domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).



use std::sync::Arc;
use ares_cordis_core::Context;
use crate::AppState;
use crate::db::audit_log;
use crate::db::schedules as db_schedules;
use crate::types::{AppError, Result};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use sha2::Digest;
use std::collections::HashMap;

pub async fn list_pipelines(
    State(ctx): State<Arc<Context>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<db_schedules::AgentPipeline>>> {
    let tenant_id = params.get("tenant_id").map(|s| s.as_str()).unwrap_or("");
    if tenant_id.is_empty() {
        return Err(AppError::InvalidInput(
            "tenant_id query param is required".into(),
        ));
    }
    let __pool_1 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::PipelineStore::new(&__pool_1);
    let pipelines = store.list_pipelines(tenant_id).await?;
    Ok(Json(pipelines))
}

pub async fn create_pipeline(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<db_schedules::CreatePipelineRequest>,
) -> Result<Json<db_schedules::AgentPipeline>> {
    let __pool_2 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::PipelineStore::new(&__pool_2);
    let pipeline = store.create_pipeline(&req).await?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let t_id = pipeline.tenant_id.clone();
    let link = format!("{} -> {}", pipeline.source_agent, pipeline.target_agent);
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "pipeline_create",
            "agent_pipeline",
            &link,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(pipeline))
}

pub async fn list_tenant_pipelines(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<db_schedules::AgentPipeline>>> {
    let __pool_3 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::PipelineStore::new(&__pool_3);
    let pipelines = store.list_pipelines(&tenant_id).await?;
    Ok(Json(pipelines))
}

pub async fn create_tenant_pipeline(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Json(mut req): Json<db_schedules::CreatePipelineRequest>,
) -> Result<Json<db_schedules::AgentPipeline>> {
    req.tenant_id = tenant_id;
    let __pool_4 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::PipelineStore::new(&__pool_4);
    let pipeline = store.create_pipeline(&req).await?;
    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let t_id = pipeline.tenant_id.clone();
    let link = format!("{} -> {}", pipeline.source_agent, pipeline.target_agent);
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "pipeline_create",
            "agent_pipeline",
            &link,
            Some(&t_id),
            None,
        )
        .await;
    });
    Ok(Json(pipeline))
}

pub async fn update_tenant_pipeline(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, id)): Path<(String, String)>,
    Json(req): Json<db_schedules::CreatePipelineRequest>,
) -> Result<Json<db_schedules::AgentPipeline>> {
    let __pool_5 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::PipelineStore::new(&__pool_5);
    let pipeline = store
        .update_pipeline(&tenant_id, &id, &req)
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!("pipeline {id} not found for tenant {tenant_id}"))
        })?;

    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let t_id = pipeline.tenant_id.clone();
    let link = format!("{} -> {}", pipeline.source_agent, pipeline.target_agent);
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "pipeline_update",
            "agent_pipeline",
            &link,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(pipeline))
}

pub async fn delete_tenant_pipeline(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let __pool_6 = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::PipelineStore::new(&__pool_6);
    let rows = store.delete_pipeline_for_tenant(&tenant_id, &id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "pipeline {id} not found for tenant {tenant_id}"
        )));
    }
    let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "pipeline_delete",
            "agent_pipeline",
            &id,
            Some(&tenant_id),
            None,
        )
        .await;
    });
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/pipelines/list_pipelines", get(list_pipelines))
        .route("/pipelines/create_pipeline", post(create_pipeline))
        .route("/pipelines/list_tenant_pipelines", get(list_tenant_pipelines))
        .route("/pipelines/create_tenant_pipeline", post(create_tenant_pipeline))
        .route("/pipelines/update_tenant_pipeline", put(update_tenant_pipeline))
        .route("/pipelines/delete_tenant_pipeline", delete(delete_tenant_pipeline))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ares_cordis_core::Service;
pub struct AdminPipelinesService;
impl Service for AdminPipelinesService {}
