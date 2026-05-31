//! V1 API handlers — tenant-scoped endpoints authenticated via API key.
//!
//! These endpoints are called by enterprise-portal and other client apps
//! using `Authorization: Bearer ares_xxx`. The `api_key_auth_middleware`
//! injects `TenantContext` into request extensions before these handlers run.

use crate::agents::context_provider::AgentRuntimeContext;
use crate::agents::tenant_agent;
use crate::db::agent_runs;
use crate::db::tenant_agents::{self, TenantAgent};
use crate::memory::estimate_tokens;
use crate::models::{TenantContext, TenantTier};
use crate::research::coordinator::ResearchCoordinator;
use crate::types::{
    AgentContext, AgentType, AppError, ChatRequest, ChatResponse, ResearchRequest,
    ResearchResponse, Result,
};
use crate::AppState;
use axum::{
    extract::{Extension, Path, Query, State},
    http::{HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};

// =============================================================================
// Response types — designed to match enterprise-portal's expected types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct V1Agent {
    pub id: String,
    pub name: String,
    pub agent_type: String,
    pub status: V1AgentStatus,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub total_runs: u64,
    pub success_rate: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum V1AgentStatus {
    Active,
    Idle,
    Error,
    Disabled,
}

impl From<TenantAgent> for V1Agent {
    fn from(a: TenantAgent) -> Self {
        let status = if a.enabled {
            V1AgentStatus::Active
        } else {
            V1AgentStatus::Disabled
        };
        Self {
            id: a.id,
            name: a.agent_name,
            agent_type: "custom".to_string(),
            status,
            config: a.config,
            created_at: ts_to_dt(a.created_at),
            last_run: None,
            total_runs: 0,
            success_rate: 0.0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct V1AgentRun {
    pub id: String,
    pub agent_id: String,
    pub status: String,
    pub input: serde_json::Value,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub tokens_used: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct V1AgentLog {
    pub id: String,
    pub agent_id: String,
    pub run_id: Option<String>,
    pub level: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub per_page: u32,
    pub total_pages: u32,
}

impl<T> Paginated<T> {
    fn empty(page: u32, per_page: u32) -> Self {
        Self {
            items: vec![],
            total: 0,
            page,
            per_page,
            total_pages: 0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct V1Usage {
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub total_runs: u64,
    pub total_tokens: u64,
    pub total_api_calls: u64,
    pub quota_runs: Option<u64>,
    pub quota_tokens: Option<u64>,
    pub daily_usage: Vec<DailyUsage>,
}

#[derive(Debug, Serialize)]
pub struct DailyUsage {
    pub date: String,
    pub runs: u64,
    pub tokens: u64,
    pub api_calls: u64,
}

#[derive(Debug, Serialize)]
pub struct V1ApiKey {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    pub last_used: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub expires_in_days: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct CreateApiKeyResponse {
    pub key: V1ApiKey,
    pub secret: String,
}

#[derive(Debug, Deserialize)]
pub struct PaginationQuery {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

// =============================================================================
// Helpers
// =============================================================================

fn ts_to_dt(ts: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now)
}

fn extract_tenant(ctx: Option<Extension<TenantContext>>) -> Result<TenantContext> {
    ctx.map(|Extension(c)| c)
        .ok_or_else(|| AppError::Auth("Missing tenant context".to_string()))
}

fn set_header(headers: &mut axum::http::HeaderMap, name: &'static str, value: impl ToString) {
    if let Ok(value) = HeaderValue::from_str(&value.to_string()) {
        headers.insert(HeaderName::from_static(name), value);
    }
}

fn usage_response<T: Serialize>(
    payload: T,
    input_tokens: u64,
    output_tokens: u64,
    model_name: &str,
    provider_name: &str,
    agent_name: &str,
) -> Response {
    let mut response = Json(payload).into_response();
    let headers = response.headers_mut();
    set_header(headers, "x-input-tokens", input_tokens);
    set_header(headers, "x-output-tokens", output_tokens);
    set_header(headers, "x-model-name", model_name);
    set_header(headers, "x-provider-name", provider_name);
    set_header(headers, "x-agent-name", agent_name);
    response
}

async fn enforce_quota(state: &AppState, tc: &TenantContext) -> Result<()> {
    if tc.tier != TenantTier::Enterprise {
        let monthly = state
            .tenant_db
            .get_monthly_requests(&tc.tenant_id)
            .await
            .unwrap_or(0);
        let daily = state
            .tenant_db
            .get_daily_requests(&tc.tenant_id)
            .await
            .unwrap_or(0);
        if !tc.can_make_request(monthly, daily) {
            return Err(AppError::RateLimited(format!(
                "Quota exceeded for {:?} tier. Monthly: {}/{}, Daily: {}/{}",
                tc.tier, monthly, tc.quota.requests_per_month, daily, tc.quota.requests_per_day
            )));
        }
    }
    Ok(())
}

// =============================================================================
// Handlers
// =============================================================================

/// POST /v1/chat — tenant-scoped chat (API key auth, no conversation history)
pub async fn v1_chat(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Json(payload): Json<ChatRequest>,
) -> Result<axum::response::Response> {
    let tc = extract_tenant(ctx)?;

    // Quota enforcement — check monthly + daily request limits
    enforce_quota(&state, &tc).await?;

    // Emergency stop — kill switch for all agents
    if state
        .emergency_stop
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(crate::types::AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        ));
    }

    // Build a minimal agent context (no user-level conversation/memory)
    let agent_context = AgentContext {
        user_id: tc.tenant_id.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        conversation_history: vec![],
        user_memory: None,
    };

    // Determine agent type
    let agent_type = if let Some(at) = payload.agent_type {
        at
    } else {
        AgentType::Orchestrator
    };

    // Execute agent with timing
    let agent_name = crate::agents::registry::AgentRegistry::type_to_name(&agent_type).to_string();
    let start = std::time::Instant::now();

    // Inject Eruka context — the core product feature.
    // Calls the ContextProvider (ErukaContextProvider in managed mode, NoOp in OSS)
    // to fetch per-agent knowledge state and gap constraints from Eruka.
    let mut runtime_context =
        AgentRuntimeContext::new(tc.tenant_id.clone(), agent_name.clone(), "api_v1_chat");
    runtime_context.workspace_id = payload.workspace_id.clone();
    runtime_context.session_id = Some(agent_context.session_id.clone());

    let eruka_context = state
        .context_provider
        .get_context_for_run(&runtime_context)
        .await;
    let eruka_context_hit = eruka_context.is_some();

    let effective_message = if let Some(ctx) = eruka_context {
        tracing::info!(
            agent = %agent_name,
            tenant = %tc.tenant_id,
            ctx_len = ctx.len(),
            "External context injected into agent call"
        );
        format!("{}\n\n---\nUser message: {}", ctx, payload.message)
    } else {
        payload.message.clone()
    };

    use crate::agents::Agent;
    let resolved_agent = tenant_agent::resolve_agent_for_tenant(
        state.tenant_db.pool(),
        &state.agent_registry,
        &tc.tenant_id,
        &agent_name,
    )
    .await?;
    let response = resolved_agent
        .agent
        .execute(&effective_message, &agent_context)
        .await?;
    let duration_ms = start.elapsed().as_millis() as i64;

    let response_text = response.content;
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

    // Use actual LLM token counts; fall back to heuristic estimates if unavailable
    let (input_tokens, output_tokens) = if let Some(u) = response.usage {
        (u.prompt_tokens, u.completion_tokens)
    } else {
        (
            estimate_tokens(&effective_message) as u32,
            estimate_tokens(&response_text) as u32,
        )
    };

    // Record agent run with real model/provider
    {
        let pool = state.tenant_db.pool().clone();
        let tid = tc.tenant_id.clone();
        let aname = resolved_agent.agent_name.clone();
        let itok = input_tokens as i64;
        let otok = output_tokens as i64;
        let mname = model_name.clone();
        let pname = provider_name.clone();
        let metadata = agent_runs::AgentRunMetadata {
            workspace_id: payload.workspace_id.clone(),
            session_id: Some(agent_context.session_id.clone()),
            request_source: Some("api_v1_chat".to_string()),
            product: None,
            agent_config_source: Some(resolved_agent.source.as_str().to_string()),
            agent_config_version: resolved_agent.config_version.clone(),
            eruka_binding_id: None,
            eruka_context_hit,
            eruka_read_count: if eruka_context_hit { 1 } else { 0 },
            eruka_write_count: 0,
        };
        tokio::spawn(async move {
            let _ = agent_runs::insert_agent_run_with_metadata(
                &pool,
                &tid,
                &aname,
                None,
                "completed",
                itok,
                otok,
                duration_ms,
                None,
                &mname,
                &pname,
                false,
                Some(&metadata),
            )
            .await;
        });
    }

    let chat_response = ChatResponse {
        response: response_text,
        agent: format!(
            "{} ({})",
            resolved_agent.agent_name,
            resolved_agent.source.as_str()
        ),
        context_id: agent_context.session_id,
        sources: None,
    };

    let mut response = usage_response(
        chat_response,
        input_tokens as u64,
        output_tokens as u64,
        &model_name,
        &provider_name,
        &resolved_agent.agent_name,
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
    if let Some(workspace_id) = &payload.workspace_id {
        set_header(
            response.headers_mut(),
            "x-runtime-workspace-id",
            workspace_id,
        );
    }
    Ok(response)
}

/// POST /v1/research — tenant-scoped research with provider-reported metering.
pub async fn v1_research(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Json(payload): Json<ResearchRequest>,
) -> Result<Response> {
    let tc = extract_tenant(ctx)?;
    enforce_quota(&state, &tc).await?;

    if state
        .emergency_stop
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        ));
    }

    let start = std::time::Instant::now();
    let config = state.config_manager.config();
    let (depth, max_iterations) = if let Some(workflow) = config.get_workflow("research") {
        (
            payload.depth.unwrap_or(workflow.max_depth),
            payload.max_iterations.unwrap_or(workflow.max_iterations),
        )
    } else {
        (
            payload.depth.unwrap_or(2),
            payload.max_iterations.unwrap_or(5),
        )
    };

    let model_key = config
        .get_agent("orchestrator")
        .map(|a| a.model.as_str())
        .unwrap_or("powerful");
    let configured_provider = config
        .get_model(model_key)
        .map(|m| m.provider.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let llm_client = match state
        .provider_registry
        .create_client_for_model(model_key)
        .await
    {
        Ok(client) => client,
        Err(_) => state.llm_factory.create_default().await?,
    };
    let model_name = llm_client.model_name().to_string();

    let coordinator = ResearchCoordinator::new(llm_client, depth, max_iterations);
    let (findings, sources, usage) = coordinator.research_with_usage(&payload.query).await?;

    let response = ResearchResponse {
        findings,
        sources,
        duration_ms: start.elapsed().as_millis() as u64,
    };

    Ok(usage_response(
        response,
        usage.input_tokens as u64,
        usage.output_tokens as u64,
        &model_name,
        &configured_provider,
        "research",
    ))
}

/// GET /v1/agents — list all agents for this tenant
pub async fn list_agents(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1Agent>>> {
    let tc = extract_tenant(ctx)?;
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(20).min(100);

    let agents = tenant_agents::list_tenant_agents(state.tenant_db.pool(), &tc.tenant_id).await?;
    let total = agents.len() as u64;
    let total_pages = ((total as f64) / (per_page as f64)).ceil() as u32;

    let start = ((page - 1) * per_page) as usize;
    let items: Vec<V1Agent> = agents
        .into_iter()
        .skip(start)
        .take(per_page as usize)
        .map(V1Agent::from)
        .collect();

    Ok(Json(Paginated {
        items,
        total,
        page,
        per_page,
        total_pages,
    }))
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
    let message = input
        .get("message")
        .or_else(|| input.get("input"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| serde_json::to_string(&input).unwrap_or_default());
    let runtime_workspace_id = input
        .get("workspace_id")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());

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
    let resolved_agent = tenant_agent::resolve_required_tenant_agent(
        state.tenant_db.pool(),
        &state.agent_registry,
        &tc.tenant_id,
        &name,
    )
    .await?;
    let mut runtime_context =
        AgentRuntimeContext::new(tc.tenant_id.clone(), name.clone(), "api_v1_agent_run");
    runtime_context.workspace_id = runtime_workspace_id.clone();
    runtime_context.session_id = Some(agent_context.session_id.clone());

    let eruka_context = state
        .context_provider
        .get_context_for_run(&runtime_context)
        .await;
    let eruka_context_hit = eruka_context.is_some();
    let effective_message = if let Some(ctx) = eruka_context {
        tracing::info!(
            agent = %name,
            tenant = %tc.tenant_id,
            ctx_len = ctx.len(),
            "External context injected into agent run"
        );
        format!("{}\n\n---\nUser message: {}", ctx, message)
    } else {
        message.clone()
    };

    let result = resolved_agent
        .agent
        .execute(&effective_message, &agent_context)
        .await;
    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(response) => {
            let (input_tokens, output_tokens) = if let Some(ref u) = response.usage {
                (u.prompt_tokens as u64, u.completion_tokens as u64)
            } else {
                (
                    estimate_tokens(&effective_message) as u64,
                    estimate_tokens(&response.content) as u64,
                )
            };

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

            // Record agent run
            let run_id = uuid::Uuid::new_v4().to_string();
            {
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
                };
                tokio::spawn(async move {
                    let _ = agent_runs::insert_agent_run_with_metadata(
                        &pool,
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
            // Record failed run
            let run_id = uuid::Uuid::new_v4().to_string();
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
                };
                tokio::spawn(async move {
                    let _ = agent_runs::insert_agent_run_with_metadata(
                        &pool,
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
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(25).min(100);
    let offset = ((page - 1) * per_page) as i64;

    let runs = agent_runs::list_agent_runs(
        state.tenant_db.pool(),
        &tc.tenant_id,
        Some(&name),
        per_page as i64,
        offset,
    )
    .await?;

    let items: Vec<V1AgentRun> = runs
        .into_iter()
        .map(|r| V1AgentRun {
            id: r.id,
            agent_id: r.agent_name,
            status: r.status,
            input: serde_json::json!({"tokens": r.input_tokens}),
            output: Some(serde_json::json!({"tokens": r.output_tokens})),
            error: r.error,
            started_at: ts_to_dt(r.created_at),
            finished_at: Some(ts_to_dt(r.created_at + (r.duration_ms / 1000))),
            duration_ms: Some(r.duration_ms as u64),
            tokens_used: Some((r.input_tokens + r.output_tokens) as u64),
        })
        .collect();

    let total = items.len() as u64;
    Ok(Json(Paginated {
        items,
        total,
        page,
        per_page,
        total_pages: ((total as f64) / (per_page as f64)).ceil() as u32,
    }))
}

/// GET /v1/agents/{name}/logs — list logs for an agent (stub: returns empty)
pub async fn list_agent_logs(
    ctx: Option<Extension<TenantContext>>,
    Path(name): Path<String>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1AgentLog>>> {
    let _tc = extract_tenant(ctx)?;
    let page = q.page.unwrap_or(1);
    let per_page = q.per_page.unwrap_or(50);
    let _ = name;
    Ok(Json(Paginated::empty(page, per_page)))
}

/// GET /v1/usage — get usage summary for this tenant
pub async fn get_usage(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
) -> Result<Json<V1Usage>> {
    let tc = extract_tenant(ctx)?;
    let summary = state.tenant_db.get_usage_summary(&tc.tenant_id).await?;

    let now = Utc::now();
    let period_start = now
        .date_naive()
        .with_day(1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();

    // Quota limits (cap u64::MAX to None for display)
    let quota_runs = if tc.quota.requests_per_month == u64::MAX {
        None
    } else {
        Some(tc.quota.requests_per_month)
    };
    let quota_tokens = if tc.quota.tokens_per_month == u64::MAX {
        None
    } else {
        Some(tc.quota.tokens_per_month)
    };

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
            expires_at: k.expires_at.map(|e| ts_to_dt(e)),
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
            expires_at: api_key.expires_at.map(|e| ts_to_dt(e)),
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

#[cfg(all(feature = "local-embeddings", feature = "ares-vector"))]
/// POST /v1/search/semantic — semantic document search
///
/// Searches ingested documents using semantic similarity.
/// Available only when `ares-vector` feature is enabled.
pub async fn semantic_search(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Json(payload): Json<crate::types::SemanticSearchRequest>,
) -> Result<Json<crate::types::SemanticSearchResponse>> {
    use crate::api::handlers::rag::{get_embedding_service, get_vector_store};
    use crate::db::VectorStore;
    use std::time::Instant;

    let start = Instant::now();
    let tc = extract_tenant(ctx)?;

    // Validate input
    if payload.collection.is_empty() {
        return Err(AppError::InvalidInput("Collection name required".into()));
    }
    if payload.query.is_empty() {
        return Err(AppError::InvalidInput("Query required".into()));
    }
    // Enforce max limit
    let limit = payload.limit.min(100).max(1);

    // Get services
    let embedding_service = get_embedding_service().await?;
    let vector_path = &state.config_manager.config().rag.vector.vector_path;
    let vector_store = get_vector_store(vector_path).await?;

    // Build scoped collection name with tenant isolation
    let scoped_collection = format!("tenant_{}_{}", tc.tenant_id, payload.collection);

    // Check collection exists
    if !vector_store.collection_exists(&scoped_collection).await? {
        return Err(AppError::NotFound(format!(
            "Collection '{}' not found",
            payload.collection
        )));
    }

    // Generate query embedding
    let query_embedding = embedding_service.embed_text(&payload.query).await?;

    // Perform vector search with cosine similarity
    let results = vector_store
        .search(
            &scoped_collection,
            &query_embedding,
            limit,
            payload.threshold,
        )
        .await?;

    // Map to response format
    let search_results: Vec<crate::types::SemanticSearchResult> = results
        .into_iter()
        .map(|r| crate::types::SemanticSearchResult {
            id: r.document.id,
            content: r.document.content,
            similarity: r.score,
            metadata: r.document.metadata,
        })
        .collect();

    let total = search_results.len();

    tracing::info!(
        tenant_id = %tc.tenant_id,
        collection = %payload.collection,
        query = %payload.query,
        results = total,
        duration_ms = start.elapsed().as_millis() as u64,
        "Semantic search completed"
    );

    Ok(Json(crate::types::SemanticSearchResponse {
        results: search_results,
        total,
        duration_ms: start.elapsed().as_millis() as u64,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use chrono::TimeZone;

    fn sample_tenant_agent(enabled: bool) -> TenantAgent {
        TenantAgent {
            id: "agent-row-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            agent_name: "support-bot".to_string(),
            display_name: "Support Bot".to_string(),
            description: Some("handles tickets".to_string()),
            config: serde_json::json!({"model": "fast"}),
            enabled,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_100,
        }
    }

    #[test]
    fn ts_to_dt_converts_valid_unix_timestamp() {
        let dt = ts_to_dt(1_700_000_000);
        assert_eq!(dt, Utc.timestamp_opt(1_700_000_000, 0).single().unwrap());
    }

    #[test]
    fn ts_to_dt_invalid_timestamp_falls_back_to_now() {
        let before = Utc::now();
        let dt = ts_to_dt(i64::MAX);
        let after = Utc::now();
        assert!(dt >= before && dt <= after);
    }

    #[test]
    fn extract_tenant_returns_context_when_present() {
        let ctx = TenantContext::new("tenant-42".into(), TenantTier::Pro);
        let got = extract_tenant(Some(Extension(ctx.clone()))).expect("tenant context");
        assert_eq!(got.tenant_id, ctx.tenant_id);
        assert_eq!(got.tier, ctx.tier);
    }

    #[test]
    fn extract_tenant_missing_context_is_auth_error() {
        let err = extract_tenant(None).unwrap_err();
        match err {
            AppError::Auth(msg) => assert!(msg.contains("Missing tenant context")),
            other => panic!("expected Auth error, got {other:?}"),
        }
    }

    #[test]
    fn set_header_inserts_valid_values() {
        let mut headers = axum::http::HeaderMap::new();
        set_header(&mut headers, "x-model-name", "gpt-test");
        assert_eq!(
            headers.get("x-model-name").and_then(|v| v.to_str().ok()),
            Some("gpt-test")
        );
    }

    #[test]
    fn set_header_skips_invalid_header_values() {
        let mut headers = axum::http::HeaderMap::new();
        set_header(&mut headers, "x-model-name", "\ninvalid");
        assert!(headers.get("x-model-name").is_none());
    }

    #[tokio::test]
    async fn usage_response_sets_metering_headers_and_json_body() {
        let response = usage_response(
            serde_json::json!({"answer": "ok"}),
            12,
            34,
            "gpt-test",
            "openai",
            "router",
        );

        assert_eq!(
            response
                .headers()
                .get("x-input-tokens")
                .and_then(|v| v.to_str().ok()),
            Some("12")
        );
        assert_eq!(
            response
                .headers()
                .get("x-output-tokens")
                .and_then(|v| v.to_str().ok()),
            Some("34")
        );
        assert_eq!(
            response
                .headers()
                .get("x-model-name")
                .and_then(|v| v.to_str().ok()),
            Some("gpt-test")
        );
        assert_eq!(
            response
                .headers()
                .get("x-provider-name")
                .and_then(|v| v.to_str().ok()),
            Some("openai")
        );
        assert_eq!(
            response
                .headers()
                .get("x-agent-name")
                .and_then(|v| v.to_str().ok()),
            Some("router")
        );

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(json["answer"], "ok");
    }

    #[test]
    fn tenant_agent_to_v1_agent_maps_enabled_status() {
        let v1: V1Agent = sample_tenant_agent(true).into();
        assert_eq!(v1.id, "agent-row-1");
        assert_eq!(v1.name, "support-bot");
        assert_eq!(v1.agent_type, "custom");
        assert!(matches!(v1.status, V1AgentStatus::Active));
        assert_eq!(v1.config, serde_json::json!({"model": "fast"}));
        assert_eq!(v1.created_at, Utc.timestamp_opt(1_700_000_000, 0).single().unwrap());
        assert!(v1.last_run.is_none());
        assert_eq!(v1.total_runs, 0);
        assert_eq!(v1.success_rate, 0.0);
    }

    #[test]
    fn tenant_agent_to_v1_agent_maps_disabled_status() {
        let v1: V1Agent = sample_tenant_agent(false).into();
        assert!(matches!(v1.status, V1AgentStatus::Disabled));
    }

    #[test]
    fn paginated_empty_serializes_zero_totals() {
        let page = Paginated::<V1Agent>::empty(2, 25);
        assert!(page.items.is_empty());
        assert_eq!(page.total, 0);
        assert_eq!(page.page, 2);
        assert_eq!(page.per_page, 25);
        assert_eq!(page.total_pages, 0);

        let json = serde_json::to_value(&page).expect("serialize paginated");
        assert_eq!(json["items"], serde_json::json!([]));
        assert_eq!(json["total"], 0);
        assert_eq!(json["page"], 2);
        assert_eq!(json["per_page"], 25);
        assert_eq!(json["total_pages"], 0);
    }

    #[test]
    fn v1_agent_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&V1AgentStatus::Active).unwrap(),
            "\"active\""
        );
        assert_eq!(
            serde_json::to_string(&V1AgentStatus::Disabled).unwrap(),
            "\"disabled\""
        );
    }

    #[test]
    fn pagination_query_deserializes_optional_fields() {
        let q: PaginationQuery =
            serde_json::from_str(r#"{"page":3,"per_page":50}"#).expect("deserialize");
        assert_eq!(q.page, Some(3));
        assert_eq!(q.per_page, Some(50));

        let defaults: PaginationQuery = serde_json::from_str("{}").expect("empty object");
        assert!(defaults.page.is_none());
        assert!(defaults.per_page.is_none());
    }

    #[test]
    fn create_api_key_request_deserializes_expiry() {
        let req: CreateApiKeyRequest = serde_json::from_str(
            r#"{"name":"ci-key","expires_in_days":30}"#,
        )
        .expect("deserialize");
        assert_eq!(req.name, "ci-key");
        assert_eq!(req.expires_in_days, Some(30));
    }

    #[test]
    fn v1_usage_round_trips_through_json() {
        let usage = V1Usage {
            period_start: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2024, 1, 31, 23, 59, 59).unwrap(),
            total_runs: 10,
            total_tokens: 5000,
            total_api_calls: 42,
            quota_runs: Some(1000),
            quota_tokens: Some(1_000_000),
            daily_usage: vec![DailyUsage {
                date: "2024-01-15".into(),
                runs: 2,
                tokens: 800,
                api_calls: 5,
            }],
        };

        let json = serde_json::to_value(&usage).expect("serialize");
        assert_eq!(json["total_runs"], 10);
        assert_eq!(json["daily_usage"][0]["date"], "2024-01-15");
    }
}
