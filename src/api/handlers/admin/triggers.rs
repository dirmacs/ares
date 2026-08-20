//! Admin triggers domain — cordis Phase6
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

pub async fn list_triggers(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<db_schedules::EventTrigger>>> {
    let tenant_id = params.get("tenant_id").map(|s| s.as_str()).unwrap_or("");
    if tenant_id.is_empty() {
        return Err(AppError::InvalidInput(
            "tenant_id query param is required".into(),
        ));
    }
    let store = db_schedules::EventTriggerStore::new(state.tenant_db.pool());
    let triggers = store.list_triggers(tenant_id).await?;
    Ok(Json(triggers))
}

pub async fn create_trigger(
    State(state): State<AppState>,
    Json(req): Json<db_schedules::CreateTriggerRequest>,
) -> Result<Json<db_schedules::EventTrigger>> {
    let store = db_schedules::EventTriggerStore::new(state.tenant_db.pool());
    let trigger = store.create_trigger(&req).await?;

    let pool = state.tenant_db.pool().clone();
    let t_id = trigger.tenant_id.clone();
    let tr_name = trigger.name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "trigger_create",
            "event_trigger",
            &tr_name,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(trigger))
}

pub async fn delete_trigger(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let store = db_schedules::EventTriggerStore::new(state.tenant_db.pool());
    let rows = store.delete_trigger(&id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!("trigger {id} not found")));
    }

    let pool = state.tenant_db.pool().clone();
    let tid = id.clone();
    tokio::spawn(async move {
        let _ =
            audit_log::log_admin_action(&pool, "trigger_delete", "event_trigger", &tid, None, None)
                .await;
    });

    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_tenant_triggers(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<db_schedules::EventTrigger>>> {
    let store = db_schedules::EventTriggerStore::new(state.tenant_db.pool());
    let triggers = store.list_triggers(&tenant_id).await?;
    Ok(Json(triggers))
}

pub async fn create_tenant_trigger(
    State(state): State<AppState>,
    Path(tenant_id): Path<String>,
    Json(mut req): Json<db_schedules::CreateTriggerRequest>,
) -> Result<Json<db_schedules::EventTrigger>> {
    req.tenant_id = tenant_id;
    let store = db_schedules::EventTriggerStore::new(state.tenant_db.pool());
    let trigger = store.create_trigger(&req).await?;

    let pool = state.tenant_db.pool().clone();
    let t_id = trigger.tenant_id.clone();
    let tr_name = trigger.name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "trigger_create",
            "event_trigger",
            &tr_name,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(trigger))
}

pub async fn update_tenant_trigger(
    State(state): State<AppState>,
    Path((tenant_id, id)): Path<(String, String)>,
    Json(mut req): Json<db_schedules::CreateTriggerRequest>,
) -> Result<Json<db_schedules::EventTrigger>> {
    req.tenant_id = tenant_id.clone();
    let store = db_schedules::EventTriggerStore::new(state.tenant_db.pool());
    let trigger = store
        .update_trigger(&tenant_id, &id, &req)
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!("trigger {id} not found for tenant {tenant_id}"))
        })?;

    let pool = state.tenant_db.pool().clone();
    let t_id = trigger.tenant_id.clone();
    let tr_name = trigger.name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "trigger_update",
            "event_trigger",
            &tr_name,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(trigger))
}

pub async fn delete_tenant_trigger(
    State(state): State<AppState>,
    Path((tenant_id, id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let store = db_schedules::EventTriggerStore::new(state.tenant_db.pool());
    let rows = store.delete_trigger_for_tenant(&tenant_id, &id).await?;
    if rows == 0 {
        return Err(AppError::NotFound(format!(
            "trigger {id} not found for tenant {tenant_id}"
        )));
    }

    let pool = state.tenant_db.pool().clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "trigger_delete",
            "event_trigger",
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
        .route("/triggers/list_triggers", get(list_triggers))
        .route("/triggers/create_trigger", post(create_trigger))
        .route("/triggers/delete_trigger", delete(delete_trigger))
        .route("/triggers/list_tenant_triggers", get(list_tenant_triggers))
        .route("/triggers/create_tenant_trigger", post(create_tenant_trigger))
        .route("/triggers/update_tenant_trigger", put(update_tenant_trigger))
        .route("/triggers/delete_tenant_trigger", delete(delete_tenant_trigger))
}

// TODO: ctx.plugin(AdminTriggersRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminTriggersService;
// impl Service for AdminTriggersService {
//     fn name(&self) -> &'static str { "admin_triggers" }
//     fn check(&self) -> bool { true }
// }