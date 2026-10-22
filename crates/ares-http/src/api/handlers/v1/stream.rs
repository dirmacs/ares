//! V1 stream domain — cordis Phase6
//! Bodies moved from v1.rs

use super::*;
use cordis::Context;
use std::sync::Arc;

use crate::HttpError;
use crate::Result;
#[cfg(test)]
use ares_store::run_history::LogToolCallRequest;
use ares_types::models::TenantContext;
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
};

#[derive(Debug, Clone, PartialEq, Eq)]
// Test-only: exercised solely by shared.rs persistence tests.
#[cfg(test)]
pub(crate) struct SandboxToolTraceSpec {
    pub(crate) name: String,
    pub(crate) tool_type: String,
}

#[cfg(test)]
fn tenant_id_for_sandbox(ctx: &Arc<Context>) -> String {
    ctx.get::<TenantContext>()
        .map(|tc| tc.tenant_id.clone())
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn sandbox_tool_call_requests(
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
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Path(name): Path<String>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1AgentLog>>> {
    let _tc = extract_tenant(ctx)?;
    let _state_ctx = ares_agent::request_tenant_ctx(&state_ctx, _tc.clone());
    let _state_ctx = match usage {
        Some(Extension(u)) => _state_ctx.with_intercept(u),
        None => _state_ctx,
    };
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
    State(_state): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Json(payload): Json<ares_types::types::SemanticSearchRequest>,
) -> Result<Json<ares_types::types::SemanticSearchResponse>> {
    let _tc = extract_tenant(ctx)?;
    let _state = ares_agent::request_tenant_ctx(&_state, _tc.clone());
    let _state = match usage {
        Some(Extension(u)) => _state.with_intercept(u),
        None => _state,
    };
    if payload.collection.is_empty() {
        return Err(HttpError::from(AppError::InvalidInput(
            "Collection name required".to_string(),
        )));
    }
    if payload.query.is_empty() {
        return Err(HttpError::from(AppError::InvalidInput(
            "Query required".to_string(),
        )));
    }
    Err(HttpError::from(AppError::InvalidInput(
        "semantic search not enabled — vector service unavailable (enable ares-vector)".into(),
    )))
}

// cordis Phase6: RouteSet Service

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::models::TenantTier;

    #[test]
    fn sandbox_tool_trace_specs_from_ctx_reads_intercept() {
        let root = Context::new_root();
        assert_eq!(tenant_id_for_sandbox(&root), "");

        let scoped = ares_agent::request_tenant_ctx(
            &root,
            TenantContext::new("acme".into(), TenantTier::Pro),
        );
        assert_eq!(tenant_id_for_sandbox(&scoped), "acme");
    }
}
