//! V1 agents domain — cordis Phase6
//! Bodies moved from v1.rs

use super::*;

use crate::agents::context_provider::AgentRuntimeContext;
use crate::agents::tenant_agent;
use crate::db::agent_runs;
use crate::db::tenant_agents::{self};
use crate::models::TenantContext;
use crate::observability::RunObservability;
use crate::types::{
    AgentContext, Result,
};
use crate::AppState;
use ares_agents::Agent;
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::Response,
    Json,
};
use chrono::{TimeZone, Utc};
use std::sync::Arc;

/// GET /v1/agents — list all agents for this tenant
pub async fn list_agents(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1Agent>>> {
    let tc = extract_tenant(ctx)?;
    let page = normalize_page(q.page);
    let per_page = normalize_per_page(q.per_page, 20);

    let agents = tenant_agents::list_tenant_agents(state.tenant_db.pool(), &tc.tenant_id).await?;
    let items: Vec<V1Agent> = agents.into_iter().map(V1Agent::from).collect();

    Ok(Json(paginate_vec(items, page, per_page)))
}

/// GET /v1/agents/{name} — get a specific agent
pub async fn get_agent(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Path(name): Path<String>,
) -> Result<Json<V1Agent>> {
    let tc = extract_tenant(ctx)?;
    let agent =
        tenant_agents::get_tenant_agent(state.tenant_db.pool(), &tc.tenant_id, &name).await?;
    Ok(Json(V1Agent::from(agent)))
}

/// POST /v1/agents/{name}/run — execute a named agent with real LLM call
pub async fn run_agent(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Path(name): Path<String>,
    Json(input): Json<serde_json::Value>,
) -> Result<Response> {
    let tc = extract_tenant(ctx)?;

    // Emergency stop
    if state
        .emergency_stop
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(crate::types::AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        ));
    }

    // Extract message from input JSON
    let message = extract_agent_run_message(&input);
    let runtime_workspace_id = extract_workspace_id(&input);

    // Build agent context
    let agent_context = AgentContext {
        user_id: tc.tenant_id.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        conversation_history: vec![],
        user_memory: None,
    };

    // Execute agent with timing
    let start = std::time::Instant::now();
    use crate::agents::Agent;
    let mut resolved_agent = tenant_agent::resolve_required_tenant_agent(
        state.tenant_db.pool(),
        &state.agent_registry,
        &tc.tenant_id,
        &name,
        &state.fleet_secrets,
    )
    .await?;

    // Skill-based agent execution
    resolved_agent
        .agent
        .set_runtime_tools(state.runtime_tool_registry.clone(), tc.tenant_id.clone());

    if let Some(config) = &resolved_agent.config {
        if let Some(skill_id) = config.get("skill_id").and_then(|v| v.as_str()) {
            let run_id = uuid::Uuid::new_v4().to_string();
            state.active_runs.start(crate::active_runs::ActiveRun {
                run_id: run_id.clone(),
                tenant_id: tc.tenant_id.clone(),
                agent_name: name.clone(),
                started_at: chrono::Utc::now().timestamp(),
                status: "running".to_string(),
                current_step: 0,
                total_steps: 0,
                last_update: chrono::Utc::now().timestamp(),
                tool_name: Some(format!("skill:{}", skill_id)),
                model: None,
                is_catchup: false,
                request_source: Some("api_v1_agent_run".to_string()),
                pipeline_id: None,
                schedule_id: None,
                trigger_id: None,
            });
            let skill_result = state
                .skill_engine
                .execute_skill(skill_id, &tc.tenant_id, input.clone(), &run_id)
                .await;
            let duration_ms = start.elapsed().as_millis() as u64;
            let skill_status = if skill_result.is_ok() {
                "completed"
            } else {
                "error"
            };
            state.active_runs.finish(&run_id, skill_status);

            // Record agent run
            {
                let pool = state.tenant_db.pool().clone();
                let tid = tc.tenant_id.clone();
                let aname = resolved_agent.agent_name.clone();
                let dur = duration_ms as i64;
                let metadata = agent_runs::AgentRunMetadata {
                    workspace_id: runtime_workspace_id.clone(),
                    session_id: Some(agent_context.session_id.clone()),
                    request_source: Some("api_v1_agent_run".to_string()),
                    product: None,
                    agent_config_source: Some(resolved_agent.source.as_str().to_string()),
                    agent_config_version: resolved_agent.config_version.clone(),
                    eruka_binding_id: None,
                    eruka_context_hit: false,
                    eruka_read_count: 0,
                    eruka_write_count: 0,
                    pipeline_id: None,
                    schedule_id: None,
                    trigger_id: None,
                };
                let status = if skill_result.is_ok() {
                    "completed"
                } else {
                    "failed"
                };
                let (input_tokens, output_tokens) = skill_result
                    .as_ref()
                    .map(crate::skill_engine::skill_result_token_counts)
                    .unwrap_or((0, 0));
                let err_msg = skill_result.as_ref().err().cloned();
                let run_id_for_insert = run_id.clone();
                tokio::spawn(async move {
                    let _ = agent_runs::insert_agent_run_with_id_and_metadata(
                        &pool,
                        &run_id_for_insert,
                        &tid,
                        &aname,
                        None,
                        status,
                        input_tokens,
                        output_tokens,
                        dur,
                        err_msg.as_deref(),
                        "skill",
                        "skill",
                        false,
                        Some(&metadata),
                    )
                    .await;
                });
            }

            let response_agent_id = resolved_agent.agent_name.clone();
            let (response, input_tokens, output_tokens) = match skill_result {
                Ok(context) => {
                    let (input_tokens, output_tokens) =
                        crate::skill_engine::skill_result_token_counts(&context);
                    let total_tokens = (input_tokens + output_tokens).max(0) as u64;
                    let response = V1AgentRun {
                        id: run_id,
                        agent_id: response_agent_id.clone(),
                        status: "completed".to_string(),
                        input: input.clone(),
                        output: Some(context),
                        error: None,
                        started_at: Utc::now(),
                        finished_at: Some(Utc::now()),
                        duration_ms: Some(duration_ms),
                        tokens_used: Some(total_tokens),
                    };
                    (
                        response,
                        input_tokens.max(0) as u64,
                        output_tokens.max(0) as u64,
                    )
                }
                Err(e) => {
                    let response = V1AgentRun {
                        id: run_id,
                        agent_id: response_agent_id.clone(),
                        status: "failed".to_string(),
                        input: input.clone(),
                        output: None,
                        error: Some(e),
                        started_at: Utc::now(),
                        finished_at: Some(Utc::now()),
                        duration_ms: Some(duration_ms),
                        tokens_used: Some(0),
                    };
                    (response, 0u64, 0u64)
                }
            };

            let mut response = usage_response(
                response,
                input_tokens,
                output_tokens,
                "skill",
                "skill",
                &response_agent_id,
            );
            set_header(
                response.headers_mut(),
                "x-agent-config-source",
                resolved_agent.source.as_str(),
            );
            if let Some(config_version) = &resolved_agent.config_version {
                set_header(
                    response.headers_mut(),
                    "x-agent-config-version",
                    config_version,
                );
            }
            if let Some(workspace_id) = &runtime_workspace_id {
                set_header(
                    response.headers_mut(),
                    "x-runtime-workspace-id",
                    workspace_id,
                );
            }
            return Ok(response);
        }
    }

    // Run observability
    let run_id = uuid::Uuid::new_v4().to_string();
    let obs = Arc::new(RunObservability {
        run_id: run_id.clone(),
        tenant_id: tc.tenant_id.clone(),
        agent_name: name.clone(),
        pool: state.tenant_db.pool().clone(),
    });
    resolved_agent.agent.set_observability(obs.clone());

    let mut runtime_context =
        AgentRuntimeContext::new(tc.tenant_id.clone(), name.clone(), "api_v1_agent_run");
    runtime_context.workspace_id = runtime_workspace_id.clone();
    runtime_context.session_id = Some(agent_context.session_id.clone());

    let eruka_context = state
        .context_provider
        .get_context_for_run(&runtime_context)
        .await;
    let eruka_context_hit = eruka_context.is_some();
    let effective_message = if let Some(ctx) = eruka_context.as_deref() {
        tracing::info!(
            agent = %name,
            tenant = %tc.tenant_id,
            ctx_len = ctx.len(),
            "External context injected into agent run"
        );
        format_message_with_context(ctx, &message)
    } else {
        message.clone()
    };

    state.active_runs.start(crate::active_runs::ActiveRun {
        run_id: run_id.clone(),
        tenant_id: tc.tenant_id.clone(),
        agent_name: name.clone(),
        started_at: chrono::Utc::now().timestamp(),
        status: "running".to_string(),
        current_step: 0,
        total_steps: 0,
        last_update: chrono::Utc::now().timestamp(),
        tool_name: None,
        model: None,
        is_catchup: false,
        request_source: Some("api_v1_agent_run".to_string()),
        pipeline_id: None,
        schedule_id: None,
        trigger_id: None,
    });
    let result = resolved_agent
        .agent
        .execute(&effective_message, &agent_context)
        .await;
    let duration_ms = start.elapsed().as_millis() as u64;

    // Aggregate run costs (fire-and-forget)
    let dur_i64 = duration_ms as i64;
    let _pool_clone = state.tenant_db.pool().clone();
    let obs_for_spawn = obs.clone();
    tokio::spawn(async move {
        obs_for_spawn.aggregate_run_cost(dur_i64).await;
    });

    match result {
        Ok(response) => {
            let (input_tokens, output_tokens) = llm_token_counts_u64(
                response.usage.as_ref(),
                &effective_message,
                &response.content,
            );

            let model_name = response
                .metadata
                .as_ref()
                .map(|m| m.model_name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            let provider_name = response
                .metadata
                .as_ref()
                .map(|m| m.provider_name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            state.active_runs.update_model(&run_id, Some(&model_name));
            state.active_runs.finish(&run_id, "completed");

            // Record agent run
            {
                let run_id_for_insert = run_id.clone();
                let pool = state.tenant_db.pool().clone();
                let tid = tc.tenant_id.clone();
                let aname = resolved_agent.agent_name.clone();
                let itok = input_tokens as i64;
                let otok = output_tokens as i64;
                let dur = duration_ms as i64;
                let mname = model_name.clone();
                let pname = provider_name.clone();
                let metadata = agent_runs::AgentRunMetadata {
                    workspace_id: runtime_workspace_id.clone(),
                    session_id: Some(agent_context.session_id.clone()),
                    request_source: Some("api_v1_agent_run".to_string()),
                    product: None,
                    agent_config_source: Some(resolved_agent.source.as_str().to_string()),
                    agent_config_version: resolved_agent.config_version.clone(),
                    eruka_binding_id: None,
                    eruka_context_hit,
                    eruka_read_count: if eruka_context_hit { 1 } else { 0 },
                    eruka_write_count: 0,
                    pipeline_id: None,
                    schedule_id: None,
                    trigger_id: None,
                };
                tokio::spawn(async move {
                    let _ = agent_runs::insert_agent_run_with_id_and_metadata(
                        &pool,
                        &run_id_for_insert,
                        &tid,
                        &aname,
                        None,
                        "completed",
                        itok,
                        otok,
                        dur,
                        None,
                        &mname,
                        &pname,
                        false,
                        Some(&metadata),
                    )
                    .await;
                });
            }

            let response_agent_id = resolved_agent.agent_name.clone();
            let response = V1AgentRun {
                id: run_id,
                agent_id: response_agent_id.clone(),
                status: "completed".to_string(),
                input,
                output: Some(serde_json::json!({"response": response.content})),
                error: None,
                started_at: Utc::now(),
                finished_at: Some(Utc::now()),
                duration_ms: Some(duration_ms),
                tokens_used: Some(input_tokens + output_tokens),
            };

            let mut response = usage_response(
                response,
                input_tokens,
                output_tokens,
                &model_name,
                &provider_name,
                &response_agent_id,
            );
            set_header(
                response.headers_mut(),
                "x-agent-config-source",
                resolved_agent.source.as_str(),
            );
            if let Some(config_version) = &resolved_agent.config_version {
                set_header(
                    response.headers_mut(),
                    "x-agent-config-version",
                    config_version,
                );
            }
            if let Some(workspace_id) = &runtime_workspace_id {
                set_header(
                    response.headers_mut(),
                    "x-runtime-workspace-id",
                    workspace_id,
                );
            }
            Ok(response)
        }
        Err(e) => {
            state.active_runs.finish(&run_id, "error");
            // Record failed run
            {
                let pool = state.tenant_db.pool().clone();
                let tid = tc.tenant_id.clone();
                let aname = resolved_agent.agent_name.clone();
                let err_msg = e.to_string();
                let dur = duration_ms as i64;
                let metadata = agent_runs::AgentRunMetadata {
                    workspace_id: runtime_workspace_id.clone(),
                    session_id: Some(agent_context.session_id.clone()),
                    request_source: Some("api_v1_agent_run".to_string()),
                    product: None,
                    agent_config_source: Some(resolved_agent.source.as_str().to_string()),
                    agent_config_version: resolved_agent.config_version.clone(),
                    eruka_binding_id: None,
                    eruka_context_hit,
                    eruka_read_count: if eruka_context_hit { 1 } else { 0 },
                    eruka_write_count: 0,
                    pipeline_id: None,
                    schedule_id: None,
                    trigger_id: None,
                };
                let run_id_for_insert = run_id.clone();
                tokio::spawn(async move {
                    let _ = agent_runs::insert_agent_run_with_id_and_metadata(
                        &pool,
                        &run_id_for_insert,
                        &tid,
                        &aname,
                        None,
                        "failed",
                        0,
                        0,
                        dur,
                        Some(&err_msg),
                        "unknown",
                        "unknown",
                        false,
                        Some(&metadata),
                    )
                    .await;
                });
            }

            let response_agent_id = resolved_agent.agent_name.clone();
            let response = V1AgentRun {
                id: run_id,
                agent_id: response_agent_id.clone(),
                status: "failed".to_string(),
                input,
                output: None,
                error: Some(e.to_string()),
                started_at: Utc::now(),
                finished_at: Some(Utc::now()),
                duration_ms: Some(duration_ms),
                tokens_used: Some(0),
            };

            let mut response =
                usage_response(response, 0, 0, "unknown", "unknown", &response_agent_id);
            set_header(
                response.headers_mut(),
                "x-agent-config-source",
                resolved_agent.source.as_str(),
            );
            if let Some(workspace_id) = &runtime_workspace_id {
                set_header(
                    response.headers_mut(),
                    "x-runtime-workspace-id",
                    workspace_id,
                );
            }
            Ok(response)
        }
    }
}

/// GET /v1/agents/{name}/runs — list runs for an agent
pub async fn list_agent_runs(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Path(name): Path<String>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1AgentRun>>> {
    let tc = extract_tenant(ctx)?;
    let page = normalize_page(q.page);
    let per_page = normalize_per_page(q.per_page, 25);
    let offset = list_runs_offset(page, per_page);

    let runs = agent_runs::list_agent_runs(
        state.tenant_db.pool(),
        &tc.tenant_id,
        Some(&name),
        per_page as i64,
        offset,
    )
    .await?;

    let items: Vec<V1AgentRun> = runs.into_iter().map(agent_run_row_to_v1).collect();

    let total = items.len() as u64;
    Ok(Json(Paginated {
        items,
        total,
        page,
        per_page,
        total_pages: compute_total_pages(total, per_page),
    }))
}

/// GET /v1/usage — get usage summary for this tenant
pub async fn get_usage(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
) -> Result<Json<V1Usage>> {
    let tc = extract_tenant(ctx)?;
    let summary = state.tenant_db.get_usage_summary(&tc.tenant_id).await?;

    let now = Utc::now();
    let period_start = usage_period_start(now);

    // Quota limits (cap u64::MAX to None for display)
    let quota_runs = quota_display_limit(tc.quota.requests_per_month);
    let quota_tokens = quota_display_limit(tc.quota.tokens_per_month);

    Ok(Json(V1Usage {
        period_start,
        period_end: now,
        total_runs: summary.monthly_requests,
        total_tokens: summary.monthly_tokens,
        total_api_calls: summary.monthly_requests,
        quota_runs,
        quota_tokens,
        daily_usage: vec![],
    }))
}

/// GET /v1/api-keys — list API keys for this tenant
pub async fn list_api_keys(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
) -> Result<Json<Vec<V1ApiKey>>> {
    let tc = extract_tenant(ctx)?;
    let keys = state.tenant_db.list_api_keys(&tc.tenant_id).await?;

    let response: Vec<V1ApiKey> = keys
        .into_iter()
        .filter(|k| k.is_active)
        .map(|k| V1ApiKey {
            id: k.id,
            name: k.name,
            prefix: k.key_prefix,
            created_at: ts_to_dt(k.created_at),
            last_used: None,
            expires_at: k.expires_at.map(ts_to_dt),
        })
        .collect();

    Ok(Json(response))
}

/// POST /v1/api-keys — create a new API key
pub async fn create_api_key(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>> {
    let tc = extract_tenant(ctx)?;
    let (api_key, raw_key) = state
        .tenant_db
        .create_api_key(&tc.tenant_id, payload.name)
        .await?;

    Ok(Json(CreateApiKeyResponse {
        key: V1ApiKey {
            id: api_key.id,
            name: api_key.name,
            prefix: api_key.key_prefix,
            created_at: ts_to_dt(api_key.created_at),
            last_used: None,
            expires_at: api_key.expires_at.map(ts_to_dt),
        },
        secret: raw_key,
    }))
}

/// DELETE /v1/api-keys/{id} — revoke an API key
pub async fn revoke_api_key(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Path(key_id): Path<String>,
) -> Result<StatusCode> {
    let tc = extract_tenant(ctx)?;
    state
        .tenant_db
        .revoke_api_key(&tc.tenant_id, &key_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// GDPR: DELETE /v1/tenant/data — purge all tenant data (usage_events, agent_runs, api_keys)
/// The tenant account itself is NOT deleted; only operational data is purged.
pub async fn delete_tenant_data(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
) -> Result<Json<serde_json::Value>> {
    let tc = extract_tenant(ctx)?;
    let tid = &tc.tenant_id;

    let pool = state.tenant_db.pool();

    let usage_rows: Vec<i64> =
        sqlx::query_scalar("DELETE FROM usage_events WHERE tenant_id = $1 RETURNING 1")
            .bind(tid)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let usage_deleted = usage_rows.len() as i64;

    let run_rows: Vec<i64> =
        sqlx::query_scalar("DELETE FROM agent_runs WHERE tenant_id = $1 RETURNING 1")
            .bind(tid)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let runs_deleted = run_rows.len() as i64;

    // Revoke all API keys (keeps account, deletes keys)
    let key_rows: Vec<i64> =
        sqlx::query_scalar("DELETE FROM api_keys WHERE tenant_id = $1 RETURNING 1")
            .bind(tid)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let keys_deleted = key_rows.len() as i64;

    // Also clear monthly cache
    let _ = sqlx::query("DELETE FROM monthly_usage_cache WHERE tenant_id = $1")
        .bind(tid)
        .execute(pool)
        .await;

    Ok(Json(serde_json::json!({
        "status": "purged",
        "tenant_id": tid,
        "usage_events_deleted": usage_deleted,
        "agent_runs_deleted": runs_deleted,
        "api_keys_revoked": keys_deleted,
        "note": "Tenant account retained. All operational data purged per GDPR Article 17."
    })))
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/v1/agents/list_agents", get(list_agents))
        .route("/v1/agents/get_agent", get(get_agent))
        .route("/v1/agents/run_agent", post(run_agent))
        .route("/v1/agents/list_agent_runs", get(list_agent_runs))
        .route("/v1/agents/get_usage", get(get_usage))
        .route("/v1/agents/list_api_keys", get(list_api_keys))
        .route("/v1/agents/create_api_key", post(create_api_key))
        .route("/v1/agents/revoke_api_key", delete(revoke_api_key))
        .route("/v1/agents/delete_tenant_data", delete(delete_tenant_data))
}

// cordis Phase6: RouteSet Service
use ares_cordis_core::Service;
pub struct V1AgentsService;
impl Service for V1AgentsService {}