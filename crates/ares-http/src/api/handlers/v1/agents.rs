//! V1 agents domain — cordis Phase6
//! Bodies moved from v1.rs

use super::*;

use crate::observability::RunObservability;
use crate::HttpError;
use crate::Result;
use ares_agent::context_provider::AgentRuntimeContext;
use ares_store::agent_runs;
use ares_store::run_history::{redact_agent_run_error, tenant_no_retain};
use ares_store::tenant_agents::{self};
use ares_types::models::TenantContext;
use ares_types::types::AgentContext;
use axum::{
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Json,
};
use chrono::{TimeZone, Utc};
use cordis::Context;
use std::sync::Arc;

/// Best-effort guard so a pre-inserted `agent_runs` parent reaches a terminal
/// state even if the handler future is dropped (client disconnect) or unwinds
/// (panic). Normal paths `disarm` after their awaited UPDATE; `Drop` only
/// fires on the abnormal paths and spawns a `failed` close-out without
/// overwriting a terminal status.
struct RunCompletionGuard {
    pool: sqlx::PgPool,
    run_id: String,
    completed: bool,
    /// No-retain flag resolved once at guard construction (async context).
    /// `Drop` is sync and must not query; it reuses this bit to pick the
    /// close-out error text. Flag-off tenants keep byte-identical behavior.
    no_retain: bool,
}

impl RunCompletionGuard {
    fn disarm(&mut self) {
        self.completed = true;
    }
}

impl Drop for RunCompletionGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let pool = self.pool.clone();
        let run_id = self.run_id.clone();
        let no_retain = self.no_retain;
        tokio::spawn(async move {
            // Infallible by construction: flag resolved at construction, the
            // marker is a constant, and the query result is discarded.
            // Flag off persists exactly 'cancelled' as before.
            let error = redact_agent_run_error(no_retain, Some("cancelled")).unwrap_or_default();
            let _ = sqlx::query(
                "UPDATE agent_runs SET status = 'failed', error = $2, updated_at = $3 WHERE id = $1 AND status = 'running'",
            )
            .bind(&run_id)
            .bind(&error)
            .bind(chrono::Utc::now().timestamp())
            .execute(&pool)
            .await;
        });
    }
}

/// GET /v1/agents — list all agents for this tenant
pub async fn list_agents(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1Agent>>> {
    let tc = extract_tenant(ctx)?;
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    let page = normalize_page(q.page);
    let per_page = normalize_per_page(q.per_page, 20);

    let agents = tenant_agents::list_tenant_agents(
        &state_ctx
            .get::<ares_store::TenantDb>()
            .expect("not provided")
            .pool()
            .clone(),
        &tc.tenant_id,
    )
    .await?;
    let items: Vec<V1Agent> = agents.into_iter().map(V1Agent::from).collect();

    Ok(Json(paginate_vec(items, page, per_page)))
}

/// GET /v1/agents/{name} — get a specific agent
pub async fn get_agent(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Path(name): Path<String>,
) -> Result<Json<V1Agent>> {
    let tc = extract_tenant(ctx)?;
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    let agent = tenant_agents::get_tenant_agent(
        &state_ctx
            .get::<ares_store::TenantDb>()
            .expect("not provided")
            .pool()
            .clone(),
        &tc.tenant_id,
        &name,
    )
    .await?;
    Ok(Json(V1Agent::from(agent)))
}

/// POST /v1/agents/{name}/run — execute a named agent with real LLM call
pub async fn run_agent(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Path(name): Path<String>,
    Json(input): Json<serde_json::Value>,
) -> Result<Response> {
    let tc = extract_tenant(ctx)?;
    // Keep the metering handle for explicit success recording on both paths.
    // Snapshot is authoritative; headers are the fallback in `track_usage`.
    let usage_ctx = usage.clone().map(|Extension(u)| u);
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = if let Some(Extension(u)) = usage {
        state_ctx.with_intercept(u)
    } else {
        state_ctx
    };

    // Emergency stop
    if state_ctx
        .get::<ares_agent::EmergencyStop>()
        .expect("not provided")
        .is_active()
    {
        return Err(HttpError::from(ares_types::types::AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        )));
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
    let state_ctx = ares_agent::tenant_scope(&state_ctx, &tc.tenant_id);
    // AR-1 fail-closed: the run route executes the tenant's own row. A
    // missing row is a typed not-found — never a fallthrough to a same-named
    // community or system agent.
    let tenant_row = match tenant_agents::get_tenant_agent(
        &state_ctx
            .get::<ares_store::TenantDb>()
            .expect("not provided")
            .pool()
            .clone(),
        &tc.tenant_id,
        &name,
    )
    .await
    {
        Ok(row) => Some(row),
        Err(ares_types::types::AppError::NotFound(_)) => {
            return Err(HttpError::from(ares_types::types::AppError::NotFound(
                format!("Agent '{}' not found for tenant '{}'", name, tc.tenant_id),
            )))
        }
        Err(e) => return Err(HttpError::from(e)),
    };
    let (config_source, config_version) = agent_config_provenance(tenant_row.as_ref());
    let skill_id = tenant_row
        .as_ref()
        .and_then(|row| row.config.get("skill_id").and_then(|v| v.as_str()));

    let run_id = uuid::Uuid::new_v4().to_string();
    let pool = state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let obs = Arc::new(RunObservability {
        run_id: run_id.clone(),
        tenant_id: tc.tenant_id.clone(),
        agent_name: name.clone(),
        pool: pool.clone(),
    });
    let setup = AgentRunSetup {
        tenant_id: tc.tenant_id,
        agent_name: name,
        input,
        message,
        agent_context,
        runtime_workspace_id,
        config_source,
        config_version,
        start,
        run_id,
        pool,
        obs,
    };

    let outcome = dispatch_agent_run(&state_ctx, &setup, skill_id).await?;
    Ok(finish_agent_run(&setup, usage_ctx.as_ref(), outcome))
}

/// Shared inputs for the two `run_agent` execution paths. The handler builds
/// it once after tenant scope and config lookup; each path owns its
/// pre-inserted run row, close-out UPDATE, and outcome.
struct AgentRunSetup {
    tenant_id: String,
    agent_name: String,
    input: serde_json::Value,
    message: String,
    agent_context: AgentContext,
    runtime_workspace_id: Option<String>,
    config_source: &'static str,
    config_version: Option<String>,
    start: std::time::Instant,
    run_id: String,
    pool: sqlx::PgPool,
    obs: Arc<RunObservability>,
}

/// One finished `run_agent` path: the wire response plus the metering fields
/// the shared tail records and stamps.
struct AgentRunOutcome {
    run: V1AgentRun,
    input_tokens: u64,
    output_tokens: u64,
    model_name: String,
    provider_name: String,
    counts_source: &'static str,
    metering_ok: bool,
    /// `x-agent-config-version` value. Every path reports the setup's
    /// version, so success and failure responses carry the same header.
    config_version_header: Option<String>,
}

/// Config provenance for the response headers and run metadata: `tenant-db`
/// when a tenant agent row exists, the system catalog otherwise.
fn agent_config_provenance(
    row: Option<&tenant_agents::TenantAgent>,
) -> (&'static str, Option<String>) {
    match row {
        Some(row) => ("tenant-db", Some(format!("tenant-db:{}", row.updated_at))),
        None => ("system", None),
    }
}

/// Dispatch one run to the skill path when the tenant config pins a skill id,
/// else the configurable-agent path.
async fn dispatch_agent_run(
    state_ctx: &Arc<Context>,
    setup: &AgentRunSetup,
    skill_id: Option<&str>,
) -> Result<AgentRunOutcome> {
    match skill_id {
        Some(skill_id) => run_skill_agent_path(state_ctx, setup, skill_id).await,
        None => run_configurable_agent_path(state_ctx, setup).await,
    }
}

/// Skill branch of `run_agent`: pre-inserted run row, no-retain guard, skill
/// engine invoke, and the exactly-one-row close-out UPDATE.
async fn run_skill_agent_path(
    state_ctx: &Arc<Context>,
    setup: &AgentRunSetup,
    skill_id: &str,
) -> Result<AgentRunOutcome> {
    state_ctx
        .get::<crate::active_runs::ActiveRuns>()
        .expect("not provided")
        .start(crate::active_runs::ActiveRun {
            run_id: setup.run_id.clone(),
            tenant_id: setup.tenant_id.clone(),
            agent_name: setup.agent_name.clone(),
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
    // Pre-insert the parent BEFORE any call row: FKs in
    // 016_run_history_detailed.sql are immediate, so the skill engine's
    // awaited insert_llm_call/insert_tool_call rows require this id first.
    let skill_metadata = agent_runs::AgentRunMetadata {
        workspace_id: setup.runtime_workspace_id.clone(),
        session_id: Some(setup.agent_context.session_id.clone()),
        request_source: Some("api_v1_agent_run".to_string()),
        product: None,
        agent_config_source: Some(setup.config_source.to_string()),
        agent_config_version: setup.config_version.clone(),
        eruka_binding_id: None,
        eruka_context_hit: false,
        eruka_read_count: 0,
        eruka_write_count: 0,
        pipeline_id: None,
        schedule_id: None,
        trigger_id: None,
    };
    agent_runs::insert_agent_run_with_id_and_metadata(
        &setup.pool,
        &setup.run_id,
        &setup.tenant_id,
        &setup.agent_name,
        None,
        "running",
        0,
        0,
        0,
        None,
        "skill",
        "skill",
        false,
        Some(&skill_metadata),
    )
    .await?;
    // Resolve no-retain once per branch; the guard reuses it in Drop and
    // the UPDATE below reuses it for the error close-out.
    let no_retain = tenant_no_retain(&setup.pool, &setup.tenant_id).await;
    let mut run_guard = RunCompletionGuard {
        pool: setup.pool.clone(),
        run_id: setup.run_id.clone(),
        completed: false,
        no_retain,
    };
    let skill_result = state_ctx
        .get::<ares_agent::skills::SkillEngine>()
        .expect("not provided")
        .execute_skill(
            skill_id,
            &setup.tenant_id,
            setup.input.clone(),
            &setup.run_id,
            state_ctx,
        )
        .await;
    let duration_ms = setup.start.elapsed().as_millis() as u64;
    let (row_status, live_status) = if skill_result.is_ok() {
        ("completed", "completed")
    } else {
        ("failed", "error")
    };
    state_ctx
        .get::<crate::active_runs::ActiveRuns>()
        .expect("not provided")
        .finish(&setup.run_id, live_status);

    // Exactly one row per run: UPDATE the pre-inserted parent.
    let (input_tokens, output_tokens) = skill_result
        .as_ref()
        .map(ares_agent::skills::skill_result_token_counts)
        .unwrap_or((0, 0));
    let err_msg =
        redact_agent_run_error(no_retain, skill_result.as_ref().err().map(String::as_str));
    let duration_ms_i64 = duration_ms as i64;
    sqlx::query(
        "UPDATE agent_runs SET status = $2, input_tokens = $3, output_tokens = $4, duration_ms = $5, error = $6, updated_at = $7 WHERE id = $1",
    )
    .bind(&setup.run_id)
    .bind(row_status)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(duration_ms_i64)
    .bind(err_msg.as_deref())
    .bind(Utc::now().timestamp())
    .execute(&setup.pool)
    .await
    .map_err(|e| HttpError::from(ares_types::types::AppError::Database(e.to_string())))?;
    run_guard.disarm();
    // Aggregate only after the parent exists; stays spawned, test polls.
    let obs_for_spawn = setup.obs.clone();
    tokio::spawn(async move {
        obs_for_spawn.aggregate_run_cost(duration_ms_i64).await;
    });

    Ok(skill_run_outcome(setup, duration_ms, skill_result))
}

/// Shape a finished skill run: nested reported usage aggregates on success;
/// failures zero the counts and carry the error text.
fn skill_run_outcome(
    setup: &AgentRunSetup,
    duration_ms: u64,
    result: std::result::Result<serde_json::Value, String>,
) -> AgentRunOutcome {
    let (raw_in, raw_out) = result
        .as_ref()
        .map(ares_agent::skills::skill_result_token_counts)
        .unwrap_or((0, 0));
    match result {
        Ok(context) => AgentRunOutcome {
            run: V1AgentRun {
                id: setup.run_id.clone(),
                agent_id: setup.agent_name.clone(),
                status: "completed".to_string(),
                input: setup.input.clone(),
                output: Some(context),
                error: None,
                reason_code: None,
                started_at: Utc::now(),
                finished_at: Some(Utc::now()),
                duration_ms: Some(duration_ms),
                tokens_used: Some((raw_in + raw_out).max(0) as u64),
            },
            input_tokens: raw_in.max(0) as u64,
            output_tokens: raw_out.max(0) as u64,
            model_name: "skill".to_string(),
            provider_name: "skill".to_string(),
            counts_source: "reported",
            metering_ok: true,
            config_version_header: setup.config_version.clone(),
        },
        Err(_error) => failed_run_outcome(setup, duration_ms, "internal_error", "skill", "skill"),
    }
}

/// Generic wire text for failed runs. The raw error survives only in the
/// database copy (raw when `no_retain` is off, the marker when it is on); the
/// body carries the class through `reason_code` and nothing else.
const FAILED_RUN_MESSAGE: &str = "The run failed. See reason_code.";

/// Coarse failure class for the wire. Local mapping, not `AppError::code()`:
/// that collapses `Unavailable` and `RateLimited` into `InternalError`, and
/// callers need the difference.
fn failure_reason_code(error: &ares_types::types::AppError) -> &'static str {
    use ares_types::types::AppError;
    match error {
        AppError::LLM(_) => "llm_error",
        AppError::External(_) => "provider_error",
        AppError::Unavailable(_) => "unavailable",
        AppError::RateLimited(_) => "rate_limited",
        _ => "internal_error",
    }
}

/// Failed-run response + metering pieces: zeroed counts, generic wire error
/// text plus the failure class, and the caller's model/provider labels. The
/// config version comes from the setup, so failures stamp the same
/// `x-agent-config-version` header as successes. HTTP stays 200: consumers
/// read `status`, which matches the documented eHB envelope behavior.
fn failed_run_outcome(
    setup: &AgentRunSetup,
    duration_ms: u64,
    reason_code: &str,
    model_name: &str,
    provider_name: &str,
) -> AgentRunOutcome {
    AgentRunOutcome {
        run: V1AgentRun {
            id: setup.run_id.clone(),
            agent_id: setup.agent_name.clone(),
            status: "failed".to_string(),
            input: setup.input.clone(),
            output: None,
            error: Some(FAILED_RUN_MESSAGE.to_string()),
            reason_code: Some(reason_code.to_string()),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            duration_ms: Some(duration_ms),
            tokens_used: Some(0),
        },
        input_tokens: 0,
        output_tokens: 0,
        model_name: model_name.to_string(),
        provider_name: provider_name.to_string(),
        counts_source: "estimated",
        metering_ok: false,
        config_version_header: setup.config_version.clone(),
    }
}

/// Configurable-agent branch of `run_agent`: ERUKA context injection, the
/// pre-inserted run row, no-retain guard, real `Execute::run`, and close-out.
async fn run_configurable_agent_path(
    state_ctx: &Arc<Context>,
    setup: &AgentRunSetup,
) -> Result<AgentRunOutcome> {
    let mut runtime_context = AgentRuntimeContext::new(
        setup.tenant_id.clone(),
        setup.agent_name.clone(),
        "api_v1_agent_run",
    );
    runtime_context.workspace_id = setup.runtime_workspace_id.clone();
    runtime_context.session_id = Some(setup.agent_context.session_id.clone());

    let (effective_message, eruka_context_hit) =
        contextual_run_message(state_ctx, setup, &runtime_context).await;

    state_ctx
        .get::<crate::active_runs::ActiveRuns>()
        .expect("not provided")
        .start(crate::active_runs::ActiveRun {
            run_id: setup.run_id.clone(),
            tenant_id: setup.tenant_id.clone(),
            agent_name: setup.agent_name.clone(),
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
    // Pre-insert the parent BEFORE exec.run: the LlmWire seam writes
    // run_llm_calls/run_tool_calls rows for this run_id during execution,
    // and those FKs are immediate. Awaited inline so no call row can race it.
    let llm_metadata = agent_runs::AgentRunMetadata {
        workspace_id: setup.runtime_workspace_id.clone(),
        session_id: Some(setup.agent_context.session_id.clone()),
        request_source: Some("api_v1_agent_run".to_string()),
        product: None,
        agent_config_source: Some(setup.config_source.to_string()),
        agent_config_version: setup.config_version.clone(),
        eruka_binding_id: None,
        eruka_context_hit,
        eruka_read_count: eruka_context_hit as i64,
        eruka_write_count: 0,
        pipeline_id: None,
        schedule_id: None,
        trigger_id: None,
    };
    agent_runs::insert_agent_run_with_id_and_metadata(
        &setup.pool,
        &setup.run_id,
        &setup.tenant_id,
        &setup.agent_name,
        None,
        "running",
        0,
        0,
        0,
        None,
        "unknown",
        "unknown",
        false,
        Some(&llm_metadata),
    )
    .await?;
    // Resolve no-retain once per branch; the guard reuses it in Drop and the
    // failed UPDATE below reuses it for the error close-out. The completed
    // UPDATE sets error = NULL and needs no change.
    let no_retain = tenant_no_retain(&setup.pool, &setup.tenant_id).await;
    let mut run_guard = RunCompletionGuard {
        pool: setup.pool.clone(),
        run_id: setup.run_id.clone(),
        completed: false,
        no_retain,
    };
    let exec = state_ctx
        .get::<ares_agent::Execute>()
        .ok_or_else(|| ares_types::types::AppError::Unavailable("Execute not provided".into()))?;
    let req = ares_agent::AgentRequest {
        agent_name: setup.agent_name.clone(),
        message: effective_message.clone(),
        history: setup.agent_context.conversation_history.clone(),
        ctx_provider: None,
        run_id: Some(setup.run_id.clone()),
        observability: Some(
            setup.obs.clone() as Arc<dyn ares_llm::observability::ObservabilitySink>
        ),
        require_tenant_agent: true,
        ..Default::default()
    };
    let result = exec
        .run(&req, state_ctx)
        .await
        .map(|exec_result| exec_result.response);
    let duration_ms = setup.start.elapsed().as_millis() as u64;

    match result {
        Ok(response) => {
            complete_llm_run(
                state_ctx,
                setup,
                &mut run_guard,
                response,
                &effective_message,
                duration_ms,
            )
            .await
        }
        Err(error) => {
            fail_llm_run(
                state_ctx,
                setup,
                &mut run_guard,
                no_retain,
                error,
                duration_ms,
            )
            .await
        }
    }
}

/// Fetch external (ERUKA) context for this run and fold it into the message.
/// Returns the effective message and whether context was injected.
async fn contextual_run_message(
    state_ctx: &Arc<Context>,
    setup: &AgentRunSetup,
    runtime_context: &AgentRuntimeContext,
) -> (String, bool) {
    let eruka_context = state_ctx
        .get::<ares_agent::ContextProviderHandle>()
        .expect("not provided")
        .0
        .get_context_for_run(runtime_context)
        .await;
    let hit = eruka_context.is_some();
    let message = if let Some(context) = eruka_context.as_deref() {
        tracing::info!(
            agent = %setup.agent_name,
            tenant = %setup.tenant_id,
            ctx_len = context.len(),
            "External context injected into agent run"
        );
        format_message_with_context(context, &setup.message)
    } else {
        setup.message.clone()
    };
    (message, hit)
}

/// Success arm of the configurable path: resolve the reported counts and
/// model, update `ActiveRuns`, and close the pre-inserted run row completed.
async fn complete_llm_run(
    state_ctx: &Arc<Context>,
    setup: &AgentRunSetup,
    guard: &mut RunCompletionGuard,
    response: ares_agent::AgentResponse,
    effective_message: &str,
    duration_ms: u64,
) -> Result<AgentRunOutcome> {
    let counts_source = llm_counts_source(response.usage.as_ref());
    let (input_tokens, output_tokens) = llm_token_counts_u64(
        response.usage.as_ref(),
        effective_message,
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
    state_ctx
        .get::<crate::active_runs::ActiveRuns>()
        .expect("not provided")
        .update_model(&setup.run_id, Some(&model_name));
    state_ctx
        .get::<crate::active_runs::ActiveRuns>()
        .expect("not provided")
        .finish(&setup.run_id, "completed");

    // Exactly one row per run: UPDATE the pre-inserted parent.
    let duration_ms_i64 = duration_ms as i64;
    sqlx::query(
        "UPDATE agent_runs SET status = 'completed', input_tokens = $2, output_tokens = $3, duration_ms = $4, error = NULL, model_name = $5, provider_name = $6, updated_at = $7 WHERE id = $1",
    )
    .bind(&setup.run_id)
    .bind(input_tokens as i64)
    .bind(output_tokens as i64)
    .bind(duration_ms_i64)
    .bind(&model_name)
    .bind(&provider_name)
    .bind(Utc::now().timestamp())
    .execute(&setup.pool)
    .await
    .map_err(|e| HttpError::from(ares_types::types::AppError::Database(e.to_string())))?;
    guard.disarm();
    // Aggregate only after the parent exists; stays spawned, test polls.
    let obs_for_spawn = setup.obs.clone();
    tokio::spawn(async move {
        obs_for_spawn.aggregate_run_cost(duration_ms_i64).await;
    });

    Ok(AgentRunOutcome {
        run: V1AgentRun {
            id: setup.run_id.clone(),
            agent_id: setup.agent_name.clone(),
            status: "completed".to_string(),
            input: setup.input.clone(),
            output: Some(serde_json::json!({"response": response.content})),
            error: None,
            reason_code: None,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            duration_ms: Some(duration_ms),
            tokens_used: Some(input_tokens + output_tokens),
        },
        input_tokens,
        output_tokens,
        model_name,
        provider_name,
        counts_source,
        metering_ok: true,
        config_version_header: setup.config_version.clone(),
    })
}

/// Failure arm of the configurable path: terminal `ActiveRuns` update, the
/// close-out row failed with the redacted error, and zeroed metering pieces.
async fn fail_llm_run(
    state_ctx: &Arc<Context>,
    setup: &AgentRunSetup,
    guard: &mut RunCompletionGuard,
    no_retain: bool,
    error: ares_types::types::AppError,
    duration_ms: u64,
) -> Result<AgentRunOutcome> {
    state_ctx
        .get::<crate::active_runs::ActiveRuns>()
        .expect("not provided")
        .finish(&setup.run_id, "error");

    // Exactly one row per run: UPDATE the pre-inserted parent.
    let raw_err = error.to_string();
    let err_msg = redact_agent_run_error(no_retain, Some(raw_err.as_str()));
    let duration_ms_i64 = duration_ms as i64;
    sqlx::query(
        "UPDATE agent_runs SET status = 'failed', input_tokens = 0, output_tokens = 0, duration_ms = $2, error = $3, updated_at = $4 WHERE id = $1",
    )
    .bind(&setup.run_id)
    .bind(duration_ms_i64)
    .bind(err_msg.as_deref())
    .bind(Utc::now().timestamp())
    .execute(&setup.pool)
    .await
    .map_err(|e| HttpError::from(ares_types::types::AppError::Database(e.to_string())))?;
    guard.disarm();
    // Aggregate only after the parent exists; stays spawned, test polls.
    let obs_for_spawn = setup.obs.clone();
    tokio::spawn(async move {
        obs_for_spawn.aggregate_run_cost(duration_ms_i64).await;
    });

    Ok(failed_run_outcome(
        setup,
        duration_ms,
        failure_reason_code(&error),
        "unknown",
        "unknown",
    ))
}

/// Shared tail of both `run_agent` paths: record the metering snapshot, build
/// the metered JSON response, and stamp the config/workspace trace headers.
fn finish_agent_run(
    setup: &AgentRunSetup,
    usage_ctx: Option<&crate::middleware::usage::UsageContext>,
    outcome: AgentRunOutcome,
) -> Response {
    if let Some(u) = usage_ctx {
        u.record(metering_snapshot(
            outcome.input_tokens as i64,
            outcome.output_tokens as i64,
            Some(outcome.model_name.clone()),
            Some(setup.agent_name.clone()),
            Some(outcome.provider_name.clone()),
            outcome.metering_ok,
            Some(outcome.counts_source.to_string()),
        ));
    }

    let mut response = usage_response(
        outcome.run,
        outcome.input_tokens,
        outcome.output_tokens,
        &outcome.model_name,
        &outcome.provider_name,
        &setup.agent_name,
        outcome.metering_ok,
        outcome.counts_source,
    );
    set_header(
        response.headers_mut(),
        "x-agent-config-source",
        setup.config_source,
    );
    if let Some(config_version) = &outcome.config_version_header {
        set_header(
            response.headers_mut(),
            "x-agent-config-version",
            config_version,
        );
    }
    if let Some(workspace_id) = &setup.runtime_workspace_id {
        set_header(
            response.headers_mut(),
            "x-runtime-workspace-id",
            workspace_id,
        );
    }
    response
}

/// GET /v1/agents/{name}/runs — list runs for an agent
pub async fn list_agent_runs(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Path(name): Path<String>,
    Query(q): Query<PaginationQuery>,
) -> Result<Json<Paginated<V1AgentRun>>> {
    let tc = extract_tenant(ctx)?;
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    let page = normalize_page(q.page);
    let per_page = normalize_per_page(q.per_page, 25);
    let offset = list_runs_offset(page, per_page);

    let runs = agent_runs::list_agent_runs(
        &state_ctx
            .get::<ares_store::TenantDb>()
            .expect("not provided")
            .pool()
            .clone(),
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
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
) -> Result<Json<V1Usage>> {
    let tc = extract_tenant(ctx)?;
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    let summary = state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .get_usage_summary(&tc.tenant_id)
        .await?;

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
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
) -> Result<Json<Vec<V1ApiKey>>> {
    let tc = extract_tenant(ctx)?;
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    let keys = state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .list_api_keys(&tc.tenant_id)
        .await?;

    let response: Vec<V1ApiKey> = keys
        .into_iter()
        .filter(|k| k.is_active)
        .map(|k| V1ApiKey {
            id: k.id,
            name: k.name,
            prefix: k.key_prefix,
            created_at: ts_to_dt(k.created_at),
            last_used: k.last_used_at.map(ts_to_dt),
            expires_at: k.expires_at.map(ts_to_dt),
            scopes: ares_types::normalize_api_key_scope(Some(&k.scopes)),
        })
        .collect();

    Ok(Json(response))
}

/// POST /v1/api-keys — create a new API key.
///
/// Honors `expires_in_days` (`1..=3650`, else 400) and `scopes`
/// (`full`/`ingest`, unknown defaults to `full`). Surfaces `expires_at`
/// and `scopes` so callers can persist them.
pub async fn create_api_key(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    headers: HeaderMap,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>> {
    let tc = extract_tenant(ctx)?;
    if let Some(days) = payload.expires_in_days {
        if !(1..=3650).contains(&days) {
            return Err(HttpError::from(ares_types::types::AppError::InvalidInput(
                "expires_in_days must be between 1 and 3650".to_string(),
            )));
        }
    }
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    let (api_key, raw_key) = state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .create_api_key(
            &tc.tenant_id,
            payload.name,
            payload.scopes,
            payload.expires_in_days,
        )
        .await?;

    // Tenant-surface mint: the key carries no user identity, so the tenant id
    // is the closest real actor; the client address comes from the forwarding
    // headers when present (Caddy sets X-Forwarded-For at the edge).
    let pool = state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let key_id = api_key.id.clone();
    let actor_id = tc.tenant_id.clone();
    let client_ip = crate::api::handlers::v1::shared::client_ip_from_headers(&headers);
    tokio::spawn(async move {
        let _ = ares_store::audit_log::log_admin_action(
            &pool,
            "create_api_key",
            "api_key",
            &key_id,
            None,
            client_ip.as_deref(),
            Some(actor_id.as_str()),
        )
        .await;
    });

    Ok(Json(CreateApiKeyResponse {
        key: V1ApiKey {
            id: api_key.id,
            name: api_key.name,
            prefix: api_key.key_prefix,
            created_at: ts_to_dt(api_key.created_at),
            last_used: api_key.last_used_at.map(ts_to_dt),
            expires_at: api_key.expires_at.map(ts_to_dt),
            scopes: ares_types::normalize_api_key_scope(Some(&api_key.scopes)),
        },
        secret: raw_key,
    }))
}

/// POST /v1/api-keys/{id}/rotate — mint a replacement key, then revoke the old.
///
/// Double-mint order: the new key is created first so the tenant never loses
/// access if the revoke fails. The old key is revoked immediately after.
/// Audit-logged. The new secret is returned once-only; it cannot be retrieved
/// again.
pub async fn rotate_api_key(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Path(key_id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<RotateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>> {
    let tc = extract_tenant(ctx)?;
    if let Some(days) = payload.expires_in_days {
        if !(1..=3650).contains(&days) {
            return Err(HttpError::from(ares_types::types::AppError::InvalidInput(
                "expires_in_days must be between 1 and 3650".to_string(),
            )));
        }
    }
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    let db = state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided");
    // Preserve the old key's name/scopes when the caller does not override.
    let old = db.get_api_key(&tc.tenant_id, &key_id).await?;
    let name = old.name.clone();
    let scopes = payload.scopes.or(Some(old.scopes.clone()));
    // Mint first.
    let (api_key, raw_key) = db
        .create_api_key(&tc.tenant_id, name, scopes, payload.expires_in_days)
        .await?;
    // Then revoke the old key.
    db.revoke_api_key(&tc.tenant_id, &key_id).await?;
    let pool = db.pool().clone();
    let new_id = api_key.id.clone();
    let old_id = key_id.clone();
    // Tenant-surface rotation: the API key carries no user identity, so the
    // tenant id is the closest real actor; the client address comes from the
    // forwarding headers when present (Caddy sets X-Forwarded-For at the edge).
    let actor_id = tc.tenant_id.clone();
    let client_ip = crate::api::handlers::v1::shared::client_ip_from_headers(&headers);
    tokio::spawn(async move {
        let details = format!("{{\"rotated_from\":\"{}\"}}", old_id);
        let _ = ares_store::audit_log::log_admin_action(
            &pool,
            "rotate_api_key",
            "api_key",
            &new_id,
            Some(&details),
            client_ip.as_deref(),
            Some(actor_id.as_str()),
        )
        .await;
    });

    Ok(Json(CreateApiKeyResponse {
        key: V1ApiKey {
            id: api_key.id,
            name: api_key.name,
            prefix: api_key.key_prefix,
            created_at: ts_to_dt(api_key.created_at),
            last_used: api_key.last_used_at.map(ts_to_dt),
            expires_at: api_key.expires_at.map(ts_to_dt),
            scopes: ares_types::normalize_api_key_scope(Some(&api_key.scopes)),
        },
        secret: raw_key,
    }))
}

/// DELETE /v1/api-keys/{id} — revoke an API key
pub async fn revoke_api_key(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Path(key_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let tc = extract_tenant(ctx)?;
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    let db = state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided");
    db.revoke_api_key(&tc.tenant_id, &key_id).await?;
    // Tenant-surface revoke: the key carries no user identity, so the tenant id
    // is the closest real actor; the client address comes from the forwarding
    // headers when present (Caddy sets X-Forwarded-For at the edge).
    let pool = db.pool().clone();
    let revoked_id = key_id.clone();
    let actor_id = tc.tenant_id.clone();
    let client_ip = crate::api::handlers::v1::shared::client_ip_from_headers(&headers);
    tokio::spawn(async move {
        let _ = ares_store::audit_log::log_admin_action(
            &pool,
            "revoke_api_key",
            "api_key",
            &revoked_id,
            None,
            client_ip.as_deref(),
            Some(actor_id.as_str()),
        )
        .await;
    });
    Ok(StatusCode::NO_CONTENT)
}

/// GDPR: DELETE /v1/tenant/data — purge all tenant data (usage_events, agent_runs, api_keys)
/// The tenant account itself is NOT deleted; only operational data is purged.
pub async fn delete_tenant_data(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
) -> Result<Json<serde_json::Value>> {
    let tc = extract_tenant(ctx)?;
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    let tid = &tc.tenant_id;

    let pool = state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();

    let usage_rows: Vec<i64> =
        sqlx::query_scalar("DELETE FROM usage_events WHERE tenant_id = $1 RETURNING 1")
            .bind(tid)
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
    let usage_deleted = usage_rows.len() as i64;

    let run_rows: Vec<i64> =
        sqlx::query_scalar("DELETE FROM agent_runs WHERE tenant_id = $1 RETURNING 1")
            .bind(tid)
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
    let runs_deleted = run_rows.len() as i64;

    // Revoke all API keys (keeps account, deletes keys)
    let key_rows: Vec<i64> =
        sqlx::query_scalar("DELETE FROM api_keys WHERE tenant_id = $1 RETURNING 1")
            .bind(tid)
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
    let keys_deleted = key_rows.len() as i64;

    // Purge tenant end-user records (email + password hash + external ids).
    // Row 7: the table is owned by the wrapper signup path; the tenant purge
    // path must reach it too.
    let user_rows: Vec<i64> =
        sqlx::query_scalar("DELETE FROM tenant_users WHERE tenant_id = $1 RETURNING 1")
            .bind(tid)
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
    let users_deleted = user_rows.len() as i64;

    // Also clear monthly cache
    let _ = sqlx::query("DELETE FROM monthly_usage_cache WHERE tenant_id = $1")
        .bind(tid)
        .execute(&pool)
        .await;

    Ok(Json(serde_json::json!({
        "status": "purged",
        "tenant_id": tid,
        "usage_events_deleted": usage_deleted,
        "agent_runs_deleted": runs_deleted,
        "api_keys_revoked": keys_deleted,
        "tenant_users_deleted": users_deleted,
        "note": "Tenant account retained. All operational and end-user data purged per GDPR Article 17."
    })))
}

pub fn routes() -> axum::Router<Arc<Context>> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/v1/agents/list_agents", get(list_agents))
        .route("/v1/agents/get_agent", get(get_agent))
        .route("/v1/agents/run_agent", post(run_agent))
        .route("/v1/agents/list_agent_runs", get(list_agent_runs))
        .route("/v1/agents/get_usage", get(get_usage))
        // External usage ingest (served today via `create_router`; repeated
        // here so the `build_routes` cutover keeps it).
        .route(
            "/v1/usage/events",
            post(super::usage_ingest::ingest_usage_events),
        )
        .route("/v1/agents/list_api_keys", get(list_api_keys))
        .route("/v1/agents/create_api_key", post(create_api_key))
        .route("/v1/agents/revoke_api_key", delete(revoke_api_key))
        .route("/v1/agents/rotate_api_key", post(rotate_api_key))
        .route("/v1/agents/delete_tenant_data", delete(delete_tenant_data))
}

// cordis Phase6: RouteSet Service
use cordis::Service;

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal setup for header-shaping tests: no database is touched because
    /// `finish_agent_run` is called with no usage context.
    fn test_setup(config_version: Option<&str>) -> AgentRunSetup {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://127.0.0.1:1/none")
            .expect("lazy pool should not connect");
        AgentRunSetup {
            tenant_id: "tenant-1".into(),
            agent_name: "agent-1".into(),
            input: serde_json::json!({"message": "hi"}),
            message: "hi".into(),
            agent_context: AgentContext {
                user_id: "user-1".into(),
                session_id: "session-1".into(),
                conversation_history: Vec::new(),
                user_memory: None,
            },
            runtime_workspace_id: None,
            config_source: "tenant-db",
            config_version: config_version.map(str::to_string),
            start: std::time::Instant::now(),
            run_id: "run-1".into(),
            pool: pool.clone(),
            obs: Arc::new(RunObservability {
                run_id: "run-1".into(),
                tenant_id: "tenant-1".into(),
                agent_name: "agent-1".into(),
                pool,
            }),
        }
    }

    fn header_value(response: &Response, name: &str) -> Option<String> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    #[tokio::test]
    async fn failed_outcome_stamps_config_version_header() {
        let setup = test_setup(Some("tenant-db:42"));
        let outcome = failed_run_outcome(&setup, 7, "llm_error", "unknown", "unknown");
        assert_eq!(
            outcome.config_version_header.as_deref(),
            Some("tenant-db:42")
        );

        let response = finish_agent_run(&setup, None, outcome);
        assert_eq!(
            header_value(&response, "x-agent-config-version").as_deref(),
            Some("tenant-db:42")
        );
    }

    #[tokio::test]
    async fn failed_outcome_omits_header_without_config_version() {
        let setup = test_setup(None);
        let outcome = failed_run_outcome(&setup, 7, "llm_error", "unknown", "unknown");
        assert_eq!(outcome.config_version_header, None);

        let response = finish_agent_run(&setup, None, outcome);
        assert_eq!(header_value(&response, "x-agent-config-version"), None);
        assert_eq!(
            header_value(&response, "x-agent-config-source").as_deref(),
            Some("tenant-db")
        );
    }

    #[test]
    fn failure_reason_codes_map_locally() {
        use ares_types::types::AppError;
        assert_eq!(failure_reason_code(&AppError::LLM("x".into())), "llm_error");
        assert_eq!(
            failure_reason_code(&AppError::External("x".into())),
            "provider_error"
        );
        assert_eq!(
            failure_reason_code(&AppError::Unavailable("x".into())),
            "unavailable"
        );
        assert_eq!(
            failure_reason_code(&AppError::RateLimited("x".into())),
            "rate_limited"
        );
        assert_eq!(
            failure_reason_code(&AppError::Internal("x".into())),
            "internal_error"
        );
        assert_eq!(
            failure_reason_code(&AppError::Database("x".into())),
            "internal_error"
        );
    }

    #[tokio::test]
    async fn failed_run_wire_body_hides_provider_text() {
        let setup = test_setup(None);
        let secret = "provider said: sk-live-12345";
        let outcome = failed_run_outcome(
            &setup,
            7,
            failure_reason_code(&ares_types::types::AppError::LLM(secret.to_string())),
            "unknown",
            "unknown",
        );

        let response = finish_agent_run(&setup, None, outcome);
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let text = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(!text.contains("sk-live-12345"));
        assert!(!text.contains("provider said"));
        let json: serde_json::Value = serde_json::from_str(&text).expect("json body");
        assert_eq!(json["status"], "failed");
        assert_eq!(json["reason_code"], "llm_error");
        assert_eq!(json["error"], FAILED_RUN_MESSAGE);
    }

    #[test]
    fn redaction_contract_for_db_copy() {
        use ares_store::run_history::redact_agent_run_error;
        assert_eq!(
            redact_agent_run_error(false, Some("raw boom")).as_deref(),
            Some("raw boom")
        );
        let redacted = redact_agent_run_error(true, Some("raw boom")).expect("redacted");
        assert_ne!(redacted, "raw boom");
        assert!(!redacted.contains("raw boom"));
    }
}
