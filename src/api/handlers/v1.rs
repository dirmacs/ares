//! V1 API handlers — tenant-scoped endpoints authenticated via API key.
//!
//! These endpoints are called by enterprise-portal and other client apps
//! using `Authorization: Bearer ares_xxx`. The `api_key_auth_middleware`
//! injects `TenantContext` into request extensions before these handlers run.

use crate::db::agent_runs;
use crate::db::tenant_agents::{self, TenantAgent};
use crate::memory::estimate_tokens;
use crate::models::{TenantContext, TenantTier};
use crate::types::{AgentContext, AgentType, AppError, ChatRequest, ChatResponse, Result};
use crate::AppState;
use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
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
    if tc.tier != TenantTier::Enterprise {
        let monthly = state.tenant_db.get_monthly_requests(&tc.tenant_id).await.unwrap_or(0);
        let daily = state.tenant_db.get_daily_requests(&tc.tenant_id).await.unwrap_or(0);
        if !tc.can_make_request(monthly, daily) {
            return Err(crate::types::AppError::RateLimited(format!(
                "Quota exceeded for {:?} tier. Monthly: {}/{}, Daily: {}/{}",
                tc.tier, monthly, tc.quota.requests_per_month, daily, tc.quota.requests_per_day
            )));
        }
    }

    // Emergency stop — kill switch for all agents
    if state.emergency_stop.load(std::sync::atomic::Ordering::Relaxed) {
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
    let eruka_context = state.context_provider
        .get_context(&agent_name, &tc.tenant_id)
        .await;

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
    let agent = state.agent_registry.create_agent(&agent_name).await?;
    let response = agent.execute(&effective_message, &agent_context).await?;
    let duration_ms = start.elapsed().as_millis() as i64;

    let response_text = response.content;
    let model_name = response.metadata.as_ref()
        .map(|m| m.model_name.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let provider_name = response.metadata.as_ref()
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
        let aname = agent_name;
        let itok = input_tokens as i64;
        let otok = output_tokens as i64;
        let mname = model_name.clone();
        let pname = provider_name.clone();
        tokio::spawn(async move {
            let _ = agent_runs::insert_agent_run(
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
            )
            .await;
        });
    }

    let chat_response = ChatResponse {
        response: response_text,
        agent: format!("{:?} (system)", agent_type),
        context_id: agent_context.session_id,
        sources: None,
    };

    let body = Json(chat_response);
    let mut response = body.into_response();
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("x-input-tokens"),
        axum::http::HeaderValue::from(input_tokens),
    );
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("x-output-tokens"),
        axum::http::HeaderValue::from(output_tokens),
    );
    if let Ok(v) = axum::http::HeaderValue::from_str(&model_name) {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static("x-model-name"), v,
        );
    }
    if let Ok(v) = axum::http::HeaderValue::from_str(&provider_name) {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static("x-provider-name"), v,
        );
    }

    Ok(response)
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
) -> Result<Json<V1AgentRun>> {
    let tc = extract_tenant(ctx)?;

    // Emergency stop
    if state.emergency_stop.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(crate::types::AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        ));
    }

    // Verify agent exists for this tenant
    let _agent =
        tenant_agents::get_tenant_agent(state.tenant_db.pool(), &tc.tenant_id, &name).await?;

    // Extract message from input JSON
    let message = input
        .get("message")
        .or_else(|| input.get("input"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| serde_json::to_string(&input).unwrap_or_default());

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
    let agent = state.agent_registry.create_agent(&name).await?;
    let result = agent.execute(&message, &agent_context).await;
    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(response) => {
            let (input_tokens, output_tokens) = if let Some(ref u) = response.usage {
                (u.prompt_tokens as u64, u.completion_tokens as u64)
            } else {
                (
                    estimate_tokens(&message) as u64,
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
                let aname = name.clone();
                let _rid = run_id.clone();
                let itok = input_tokens as i64;
                let otok = output_tokens as i64;
                let dur = duration_ms as i64;
                let mname = model_name.clone();
                let pname = provider_name.clone();
                tokio::spawn(async move {
                    let _ = agent_runs::insert_agent_run(
                        &pool, &tid, &aname, None, "completed", itok, otok, dur, None,
                        &mname, &pname, false,
                    )
                    .await;
                });
            }

            Ok(Json(V1AgentRun {
                id: run_id,
                agent_id: name,
                status: "completed".to_string(),
                input,
                output: Some(serde_json::json!({"response": response.content})),
                error: None,
                started_at: Utc::now(),
                finished_at: Some(Utc::now()),
                duration_ms: Some(duration_ms),
                tokens_used: Some(input_tokens + output_tokens),
            }))
        }
        Err(e) => {
            // Record failed run
            let run_id = uuid::Uuid::new_v4().to_string();
            {
                let pool = state.tenant_db.pool().clone();
                let tid = tc.tenant_id.clone();
                let aname = name.clone();
                let err_msg = e.to_string();
                let dur = duration_ms as i64;
                tokio::spawn(async move {
                    let _ = agent_runs::insert_agent_run(
                        &pool, &tid, &aname, None, "failed", 0, 0, dur,
                        Some(&err_msg), "unknown", "unknown", false,
                    )
                    .await;
                });
            }

            Ok(Json(V1AgentRun {
                id: run_id,
                agent_id: name,
                status: "failed".to_string(),
                input,
                output: None,
                error: Some(e.to_string()),
                started_at: Utc::now(),
                finished_at: Some(Utc::now()),
                duration_ms: Some(duration_ms),
                tokens_used: Some(0),
            }))
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
    let vector_path = &state.config_manager.config().rag.vector_path;
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
        .search(&scoped_collection, &query_embedding, limit, payload.threshold)
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

    let usage_rows: Vec<i64> = sqlx::query_scalar(
        "DELETE FROM usage_events WHERE tenant_id = $1 RETURNING 1"
    )
    .bind(tid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let usage_deleted = usage_rows.len() as i64;

    let run_rows: Vec<i64> = sqlx::query_scalar(
        "DELETE FROM agent_runs WHERE tenant_id = $1 RETURNING 1"
    )
    .bind(tid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let runs_deleted = run_rows.len() as i64;

    // Revoke all API keys (keeps account, deletes keys)
    let key_rows: Vec<i64> = sqlx::query_scalar(
        "DELETE FROM api_keys WHERE tenant_id = $1 RETURNING 1"
    )
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

