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

    let allowlist_store =
        crate::db::tenant_allowlist::TenantAllowlistStore::new(state.tenant_db.pool());
    if !allowlist_store
        .is_rag_source_allowed(&tc.tenant_id, &payload.collection)
        .await?
    {
        return Err(AppError::Auth(format!(
            "RAG source '{}' is not allowed for this tenant",
            payload.collection
        )));
    }

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

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{get, post};
    let router = axum::Router::new()
        .route("/v1/stream/sandbox_run_agent", post(sandbox_run_agent))
        .route("/v1/stream/list_agent_logs", get(list_agent_logs))
    ;
    #[cfg(all(feature = "local-embeddings", feature = "ares-vector"))]
    {
        router = router.route("/v1/stream/semantic_search", post(semantic_search));
    }
    router
}

// Service stub for v1_stream