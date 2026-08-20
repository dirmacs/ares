//! Admin pipelines domain — cordis Phase6
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

pub async fn list_pipelines(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<db_schedules::AgentPipeline>>> {
    let tenant_id = params.get("tenant_id").map(|s| s.as_str()).unwrap_or("");
    if tenant_id.is_empty() {
        return Err(AppError::InvalidInput(
            "tenant_id query param is required".into(),
        ));
    }
    let store = db_schedules::PipelineStore::new(state.tenant_db.pool());
    let pipelines = store.list_pipelines(tenant_id).await?;
    Ok(Json(pipelines))
}

pub async fn create_pipeline(
    State(state): State<AppState>,
    Json(req): Json<db_schedules::CreatePipelineRequest>,
) -> Result<Json<db_schedules::AgentPipeline>> {
    let store = db_schedules::PipelineStore::new(state.tenant_db.pool());
    let pipeline = store.create_pipeline(&req).await?;

    let pool = state.tenant_db.pool().clone();
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
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<db_schedules::AgentPipeline>>> {
    let store = db_schedules::PipelineStore::new(state.tenant_db.pool());
    let pipelines = store.list_pipelines(&tenant_id).await?;
    Ok(Json(pipelines))
}

pub async fn create_tenant_pipeline(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Json(mut req): Json<db_schedules::CreatePipelineRequest>,
) -> Result<Json<db_schedules::AgentPipeline>> {
    req.tenant_id = tenant_id;
    let store = db_schedules::PipelineStore::new(state.tenant_db.pool());
    let pipeline = store.create_pipeline(&req).await?;
    let pool = state.tenant_db.pool().clone();
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
    State(state): State<AppState>,
    Path((tenant_id, id)): Path<(String, String)>,
    Json(req): Json<db_schedules::CreatePipelineRequest>,
) -> Result<Json<db_schedules::AgentPipeline>> {
    let store = db_schedules::PipelineStore::new(state.tenant_db.pool());
    let pipeline = store
        .update_pipeline(&tenant_id, &id, &req)
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!("pipeline {id} not found for tenant {tenant_id}"))
        })?;

    let pool = state.tenant_db.pool().clone();
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
    State(state): State<AppState>,
    Path((tenant_id, id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let store = db_schedules::PipelineStore::new(state.tenant_db.pool());
    let rows = store.delete_pipeline_for_tenant(&tenant_id, &id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "pipeline {id} not found for tenant {tenant_id}"
        )));
    }
    let pool = state.tenant_db.pool().clone();
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

// TODO: ctx.plugin(AdminPipelinesRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminPipelinesService;
// impl Service for AdminPipelinesService {
//     fn name(&self) -> &'static str { "admin_pipelines" }
//     fn check(&self) -> bool { true }
// }