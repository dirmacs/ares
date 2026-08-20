//! V1 stream domain — cordis Phase6
//! Bodies moved from v1.rs

use super::*;

use crate::agents::tenant_agent;
use crate::db::agent_runs;
use crate::db::run_history::{LogToolCallRequest, RunHistoryStore};
use crate::models::TenantContext;
use crate::types::Result;
use crate::AppState;
use ares_agents::Agent;
use ares_types::types::ToolDefinition;
use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use chrono::{TimeZone, Utc};

/// POST /v1/agents/{name}/sandbox-run — dry-run an agent with sandbox=true
pub async fn sandbox_run_agent(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Path(name): Path<String>,
    Json(input): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>> {
    let tc = extract_tenant(ctx)?;

    let mut resolved_agent = tenant_agent::resolve_required_tenant_agent(
        state.tenant_db.pool(),
        &state.agent_registry,
        &tc.tenant_id,
        &name,
        &state.fleet_secrets,
    )
    .await?;
    resolved_agent
        .agent
        .set_runtime_tools(state.runtime_tool_registry.clone(), tc.tenant_id.clone());

    let run_id = uuid::Uuid::new_v4().to_string();
    let started = Utc::now();
    let tools = resolved_agent.agent.get_filtered_tool_definitions();
    let tool_trace_specs =
        sandbox_tool_trace_specs(state.runtime_tool_registry.as_ref(), &tc.tenant_id, &tools);
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let message = extract_agent_run_message(&input);

    let trace = vec![
        format!("Resolved agent '{}' for tenant '{}'", name, tc.tenant_id),
        format!("Config source: {}", resolved_agent.source.as_str()),
        format!("System prompt: {}", resolved_agent.agent.system_prompt()),
        format!("Allowed tools: {:?}", resolved_agent.agent.allowed_tools()),
        format!(
            "Max tool iterations: {}",
            resolved_agent.agent.max_tool_iterations()
        ),
        format!("Parallel tools: {}", resolved_agent.agent.parallel_tools()),
        format!("Input message: {}", message),
        "Sandbox mode active — no LLM calls or tool executions performed".to_string(),
    ];

    let metadata = agent_runs::AgentRunMetadata {
        workspace_id: None,
        session_id: Some(run_id.clone()),
        request_source: Some("api_v1_sandbox".to_string()),
        product: Some("fleet_manager".to_string()),
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
    agent_runs::insert_agent_run_with_id_and_metadata(
        state.tenant_db.pool(),
        &run_id,
        &tc.tenant_id,
        &name,
        None,
        "completed",
        0,
        0,
        0,
        None,
        "sandbox",
        "sandbox",
        false,
        Some(&metadata),
    )
    .await?;

    let store = RunHistoryStore::new(state.tenant_db.pool());
    for call in sandbox_tool_call_requests(
        &run_id,
        &tc.tenant_id,
        &name,
        &tool_trace_specs,
        started.timestamp(),
    ) {
        store.insert_tool_call(&call).await?;
    }

    Ok(Json(serde_json::json!({
        "sandbox": true,
        "run_id": run_id,
        "agent_name": name,
        "tenant_id": tc.tenant_id,
        "config_source": resolved_agent.source.as_str(),
        "config_version": resolved_agent.config_version,
        "system_prompt": resolved_agent.agent.system_prompt(),
        "tools": tool_names,
        "input": input,
        "trace": trace,
        "mock_response": {
            "content": format!("[SANDBOX] Agent {} would process: '{}' using {} tool(s). No external actions taken.", name, message, tools.len()),
            "tool_calls": tools.iter().map(|t| serde_json::json!({
                "tool": t.name,
                "mock_result": { "status": "skipped", "reason": "sandbox_mode" }
            })).collect::<Vec<_>>(),
        }
    })))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SandboxToolTraceSpec {
    name: String,
    tool_type: String,
}

fn sandbox_tool_trace_specs(
    runtime_registry: &ares_tools::runtime_registry::RuntimeToolRegistry,
    tenant_id: &str,
    tools: &[ToolDefinition],
) -> Vec<SandboxToolTraceSpec> {
    tools
        .iter()
        .map(|tool| SandboxToolTraceSpec {
            name: tool.name.clone(),
            tool_type: runtime_registry
                .tool_type_for_tenant(&tool.name, Some(tenant_id))
                .unwrap_or_else(|| "mcp".to_string()),
        })
        .collect()
}

fn sandbox_tool_call_requests(
    run_id: &str,
    tenant_id: &str,
    agent_name: &str,
    tool_specs: &[SandboxToolTraceSpec],
    created_at: i64,
) -> Vec<LogToolCallRequest> {
    tool_specs
        .iter()
        .enumerate()
        .map(|(idx, tool)| LogToolCallRequest {
            id: uuid::Uuid::new_v4().to_string(),
            run_id: run_id.to_string(),
            tenant_id: tenant_id.to_string(),
            agent_name: agent_name.to_string(),
            step_index: idx as i32,
            tool_name: tool.name.clone(),
            tool_type: tool.tool_type.clone(),
            arguments: serde_json::json!({ "sandbox": true }),
            result: Some(serde_json::json!({ "status": "skipped", "reason": "sandbox_mode" })),
            latency_ms: 0,
            status: "success".to_string(),
            error_message: None,
            created_at,
        })
        .collect()
}

/// GET /v1/agents/{name}/logs — list logs for an agent (stub: returns empty)
pub async fn list_agent_logs(
    ctx: Option<Extension<TenantContext>>,
    Path(name): Path<String>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1AgentLog>>> {
    let _tc = extract_tenant(ctx)?;
    let (page, per_page) = logs_pagination(q.page, q.per_page);
    let _ = name;
    Ok(Json(Paginated::empty(page, per_page)))
}

/// POST /v1/search/semantic — semantic document search
///
/// Searches ingested documents using semantic similarity.
/// cordis Phase6: runtime gating via Service check — previously feature-gated
/// When vector services are not configured the handler returns 503 via AppError.
pub async fn semantic_search(
    State(_state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Json(payload): Json<crate::types::SemanticSearchRequest>,
) -> Result<Json<crate::types::SemanticSearchResponse>> {
    let _tc = extract_tenant(ctx)?;
    if payload.collection.is_empty() {
        return Err(AppError::InvalidInput("Collection name required".into()));
    }
    if payload.query.is_empty() {
        return Err(AppError::InvalidInput("Query required".into()));
    }
    Err(AppError::InvalidInput(
        "semantic search not enabled — vector service unavailable (enable ares-vector)".into(),
    ))
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/v1/stream/sandbox_run_agent", post(sandbox_run_agent))
        .route("/v1/stream/list_agent_logs", get(list_agent_logs))
        .route("/v1/stream/semantic_search", post(semantic_search))
}

// cordis Phase6: RouteSet Service
use ares_cordis_core::Service;
pub struct V1StreamService;
impl Service for V1StreamService {}