//! Admin agents domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use super::*;


use crate::AppState;
use crate::agents::context_provider::AgentRuntimeContext;
use crate::agents::tenant_agent;
use crate::db::agent_runs;
use crate::db::agent_versions;
use crate::db::audit_log;
use crate::db::tenant_agents::{
    AgentTemplate, AgentTemplateStore, CreateTemplateRequest, CreateTenantAgentRequest,
    TenantAgent, UpdateTenantAgentRequest,
    create_tenant_agent as db_create_tenant_agent, delete_tenant_agent as db_delete_tenant_agent,
    get_tenant_agent as db_get_tenant_agent, list_agent_templates, list_tenant_agent_versions,
    list_tenant_agents as db_list_tenant_agents, record_tenant_agent_version,
    rollback_tenant_agent_version, update_tenant_agent as db_update_tenant_agent,
};
use crate::memory::estimate_tokens;
use crate::types::{AgentContext, AppError, Result};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::Arc;

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
    validate_agent_config_tools(
        &req.config,
        &state.tool_registry,
        &state.runtime_tool_registry,
        &tenant_id,
    )?;

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
    if let Some(cfg) = &req.config {
        validate_agent_config_tools(
            cfg,
            &state.tool_registry,
            &state.runtime_tool_registry,
            &tenant_id,
        )?;
    }

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

pub async fn list_agents(
    State(state): State<AppState>,
) -> Result<Json<Vec<agent_runs::AllAgentsEntry>>> {
    let agents = agent_runs::list_all_agents(state.tenant_db.pool()).await?;
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

    validate_agent_config_tools(
        &config,
        &state.tool_registry,
        &state.runtime_tool_registry,
        &req.tenant_id,
    )?;

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
    if let Some(cfg) = &req.config {
        validate_agent_config_tools(
            cfg,
            &state.tool_registry,
            &state.runtime_tool_registry,
            &tenant_id,
        )?;
    }

    let db_req = UpdateTenantAgentRequest {
        display_name: req.display_name,
        description: req.description,
        config: req.config,
        enabled: req.enabled,
    };
    let agent =
        db_update_tenant_agent(state.tenant_db.pool(), &tenant_id, &agent_name, db_req).await?;

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
        .create_agent_from_config_with_fallbacks(
            &agent_name,
            &agent_config,
            &tenant_id,
            state.tenant_db.pool(),
            &state.fleet_secrets,
        )
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
    draft_agent.set_runtime_tools(state.runtime_tool_registry.clone(), tenant_id.clone());
    draft_agent.set_run_id(run_id.clone());

    state.active_runs.start(crate::active_runs::ActiveRun {
        run_id: run_id.clone(),
        tenant_id: tenant_id.clone(),
        agent_name: agent_name.clone(),
        started_at: chrono::Utc::now().timestamp(),
        status: "running".to_string(),
        current_step: 0,
        total_steps: 0,
        last_update: chrono::Utc::now().timestamp(),
        tool_name: None,
        model: None,
        is_catchup: false,
        request_source: Some("admin_test_agent".to_string()),
        pipeline_id: None,
        schedule_id: None,
        trigger_id: None,
    });

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
            state.active_runs.finish(&run_id, "completed");
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
        Err(error) => {
            state.active_runs.finish(&run_id, "error");
            Ok(Json(TestTenantAgentResponse {
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
            }))
        }
    }
}

pub async fn list_agent_templates_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<AgentTemplate>>> {
    let product_type = params.get("product_type").map(|s| s.as_str());
    let templates = list_agent_templates(state.tenant_db.pool(), product_type).await?;
    Ok(Json(templates))
}

/// GET /api/admin/agents/{agent_id}/versions

/// POST /api/admin/agents/{agent_id}/rollback/{version}

/// GET /api/admin/agents/emergency-stop
/// Return whether the global emergency stop is active.
pub async fn get_emergency_stop_handler(
    State(state): State<AppState>,
) -> Result<Json<EmergencyStopStatus>> {
    Ok(Json(emergency_stop_status(
        state
            .emergency_stop
            .load(std::sync::atomic::Ordering::Relaxed),
    )))
}

/// POST /api/admin/agents/emergency-stop
/// Enable or disable the global emergency stop.
/// When active, agent execution entrypoints are rejected with 503.
pub async fn emergency_stop_handler(
    State(state): State<AppState>,
    Json(payload): Json<EmergencyStopRequest>,
) -> Result<Json<EmergencyStopStatus>> {
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

    Ok(Json(emergency_stop_status(payload.active)))
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/agents/list_tenant_agents_handler", get(list_tenant_agents_handler))
        .route("/agents/create_tenant_agent_handler", post(create_tenant_agent_handler))
        .route("/agents/update_tenant_agent_handler", put(update_tenant_agent_handler))
        .route("/agents/delete_tenant_agent_handler", delete(delete_tenant_agent_handler))
        .route("/agents/list_tenant_agent_versions_handler", get(list_tenant_agent_versions_handler))
        .route("/agents/rollback_tenant_agent_version_handler", post(rollback_tenant_agent_version_handler))
        .route("/agents/list_agents", get(list_agents))
        .route("/agents/get_agent", get(get_agent))
        .route("/agents/create_agent", post(create_agent))
        .route("/agents/update_agent", put(update_agent))
        .route("/agents/delete_agent", delete(delete_agent))
        .route("/agents/get_agent_versions", get(get_agent_versions))
        .route("/agents/rollback_agent", post(rollback_agent))
        .route("/agents/create_agent_template_handler", post(create_agent_template_handler))
        .route("/agents/delete_agent_template_handler", delete(delete_agent_template_handler))
        .route("/agents/test_tenant_agent_handler", post(test_tenant_agent_handler))
        .route("/agents/list_agent_templates_handler", get(list_agent_templates_handler))
        .route("/agents/list_agent_versions_handler", get(list_agent_versions_handler))
        .route("/agents/rollback_agent_handler", post(rollback_agent_handler))
        .route("/agents/get_emergency_stop_handler", get(get_emergency_stop_handler))
        .route("/agents/emergency_stop_handler", post(emergency_stop_handler))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ares_cordis_core::Service;
pub struct AdminAgentsService;
impl Service for AdminAgentsService {}