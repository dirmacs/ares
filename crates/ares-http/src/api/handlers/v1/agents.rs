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
    http::StatusCode,
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
                "UPDATE agent_runs SET status = 'failed', error = $2 WHERE id = $1 AND status = 'running'",
            )
            .bind(&run_id)
            .bind(&error)
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
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
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
    let tenant_row = tenant_agents::get_tenant_agent(
        &state_ctx
            .get::<ares_store::TenantDb>()
            .expect("not provided")
            .pool()
            .clone(),
        &tc.tenant_id,
        &name,
    )
    .await
    .ok();
    let config_source = if tenant_row.is_some() {
        "tenant-db"
    } else {
        "system"
    };
    let config_version = tenant_row
        .as_ref()
        .map(|row| format!("tenant-db:{}", row.updated_at));

    // Skill-based agent execution
    if let Some(skill_id) = tenant_row
        .as_ref()
        .and_then(|row| row.config.get("skill_id").and_then(|v| v.as_str()))
    {
        let run_id = uuid::Uuid::new_v4().to_string();
        state_ctx
            .get::<crate::active_runs::ActiveRuns>()
            .expect("not provided")
            .start(crate::active_runs::ActiveRun {
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
        // Pre-insert the parent BEFORE any call row: FKs in
        // 016_run_history_detailed.sql are immediate, so the skill engine's
        // awaited insert_llm_call/insert_tool_call rows require this id first.
        let skill_metadata = agent_runs::AgentRunMetadata {
            workspace_id: runtime_workspace_id.clone(),
            session_id: Some(agent_context.session_id.clone()),
            request_source: Some("api_v1_agent_run".to_string()),
            product: None,
            agent_config_source: Some(config_source.to_string()),
            agent_config_version: config_version.clone(),
            eruka_binding_id: None,
            eruka_context_hit: false,
            eruka_read_count: 0,
            eruka_write_count: 0,
            pipeline_id: None,
            schedule_id: None,
            trigger_id: None,
        };
        let pool = state_ctx
            .get::<ares_store::TenantDb>()
            .expect("not provided")
            .pool()
            .clone();
        agent_runs::insert_agent_run_with_id_and_metadata(
            &pool,
            &run_id,
            &tc.tenant_id,
            &name,
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
        let no_retain = tenant_no_retain(&pool, &tc.tenant_id).await;
        let mut run_guard = RunCompletionGuard {
            pool: pool.clone(),
            run_id: run_id.clone(),
            completed: false,
            no_retain,
        };
        let obs = Arc::new(RunObservability {
            run_id: run_id.clone(),
            tenant_id: tc.tenant_id.clone(),
            agent_name: name.clone(),
            pool: pool.clone(),
        });
        let skill_result = state_ctx
            .get::<ares_agent::skills::SkillEngine>()
            .expect("not provided")
            .execute_skill(skill_id, &tc.tenant_id, input.clone(), &run_id, &state_ctx)
            .await;
        let duration_ms = start.elapsed().as_millis() as u64;
        let skill_status = if skill_result.is_ok() {
            "completed"
        } else {
            "error"
        };
        state_ctx
            .get::<crate::active_runs::ActiveRuns>()
            .expect("not provided")
            .finish(&run_id, skill_status);

        // Exactly one row per run: UPDATE the pre-inserted parent.
        {
            let dur = duration_ms as i64;
            let status = if skill_result.is_ok() {
                "completed"
            } else {
                "failed"
            };
            let (input_tokens, output_tokens) = skill_result
                .as_ref()
                .map(ares_agent::skills::skill_result_token_counts)
                .unwrap_or((0, 0));
            let err_msg =
                redact_agent_run_error(no_retain, skill_result.as_ref().err().map(String::as_str));
            sqlx::query(
                "UPDATE agent_runs SET status = $2, input_tokens = $3, output_tokens = $4, duration_ms = $5, error = $6 WHERE id = $1",
            )
            .bind(&run_id)
            .bind(status)
            .bind(input_tokens)
            .bind(output_tokens)
            .bind(dur)
            .bind(err_msg.as_deref())
            .execute(&pool)
            .await
            .map_err(|e| HttpError::from(ares_types::types::AppError::Database(e.to_string())))?;
            run_guard.disarm();
            // Aggregate only after the parent exists; stays spawned, test polls.
            let obs_for_spawn = obs.clone();
            tokio::spawn(async move {
                obs_for_spawn.aggregate_run_cost(dur).await;
            });
        }

        let response_agent_id = name.clone();
        let (response, input_tokens, output_tokens) = match skill_result {
            Ok(context) => {
                let (input_tokens, output_tokens) =
                    ares_agent::skills::skill_result_token_counts(&context);
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
            config_source,
        );
        if let Some(config_version) = &config_version {
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

    // Run observability: the sink writes run_llm_calls/run_tool_calls rows keyed
    // by run_id, so the agent_runs parent must exist first (FKs immediate).
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
    let mut runtime_context =
        AgentRuntimeContext::new(tc.tenant_id.clone(), name.clone(), "api_v1_agent_run");
    runtime_context.workspace_id = runtime_workspace_id.clone();
    runtime_context.session_id = Some(agent_context.session_id.clone());

    let eruka_context = state_ctx
        .get::<ares_agent::ContextProviderHandle>()
        .expect("not provided")
        .0
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

    state_ctx
        .get::<crate::active_runs::ActiveRuns>()
        .expect("not provided")
        .start(crate::active_runs::ActiveRun {
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
    // Pre-insert the parent BEFORE exec.run: the LlmWire seam writes
    // run_llm_calls/run_tool_calls rows for this run_id during execution,
    // and those FKs are immediate. Awaited inline so no call row can race it.
    let llm_metadata = agent_runs::AgentRunMetadata {
        workspace_id: runtime_workspace_id.clone(),
        session_id: Some(agent_context.session_id.clone()),
        request_source: Some("api_v1_agent_run".to_string()),
        product: None,
        agent_config_source: Some(config_source.to_string()),
        agent_config_version: config_version.clone(),
        eruka_binding_id: None,
        eruka_context_hit,
        eruka_read_count: if eruka_context_hit { 1 } else { 0 },
        eruka_write_count: 0,
        pipeline_id: None,
        schedule_id: None,
        trigger_id: None,
    };
    agent_runs::insert_agent_run_with_id_and_metadata(
        &pool,
        &run_id,
        &tc.tenant_id,
        &name,
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
    let no_retain = tenant_no_retain(&pool, &tc.tenant_id).await;
    let mut run_guard = RunCompletionGuard {
        pool: pool.clone(),
        run_id: run_id.clone(),
        completed: false,
        no_retain,
    };
    let exec = state_ctx
        .get::<ares_agent::Execute>()
        .ok_or_else(|| ares_types::types::AppError::Unavailable("Execute not provided".into()))?;
    let req = ares_agent::AgentRequest {
        agent_name: name.clone(),
        message: effective_message.clone(),
        history: agent_context.conversation_history.clone(),
        ctx_provider: None,
        run_id: Some(run_id.clone()),
        observability: Some(obs.clone() as Arc<dyn ares_llm::observability::ObservabilitySink>),
        ..Default::default()
    };
    let result = exec
        .run(&req, &state_ctx)
        .await
        .map(|exec_result| exec_result.response);
    let duration_ms = start.elapsed().as_millis() as u64;

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
            state_ctx
                .get::<crate::active_runs::ActiveRuns>()
                .expect("not provided")
                .update_model(&run_id, Some(&model_name));
            state_ctx
                .get::<crate::active_runs::ActiveRuns>()
                .expect("not provided")
                .finish(&run_id, "completed");

            // Exactly one row per run: UPDATE the pre-inserted parent.
            {
                let itok = input_tokens as i64;
                let otok = output_tokens as i64;
                let dur = duration_ms as i64;
                sqlx::query(
                    "UPDATE agent_runs SET status = 'completed', input_tokens = $2, output_tokens = $3, duration_ms = $4, error = NULL, model_name = $5, provider_name = $6 WHERE id = $1",
                )
                .bind(&run_id)
                .bind(itok)
                .bind(otok)
                .bind(dur)
                .bind(&model_name)
                .bind(&provider_name)
                .execute(&pool)
                .await
                .map_err(|e| HttpError::from(ares_types::types::AppError::Database(e.to_string())))?;
                run_guard.disarm();
                // Aggregate only after the parent exists; stays spawned, test polls.
                let obs_for_spawn = obs.clone();
                tokio::spawn(async move {
                    obs_for_spawn.aggregate_run_cost(dur).await;
                });
            }

            let response_agent_id = name.clone();
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
                config_source,
            );
            if let Some(config_version) = &config_version {
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
            state_ctx
                .get::<crate::active_runs::ActiveRuns>()
                .expect("not provided")
                .finish(&run_id, "error");
            // Exactly one row per run: UPDATE the pre-inserted parent.
            {
                let raw_err = e.to_string();
                let err_msg = redact_agent_run_error(no_retain, Some(raw_err.as_str()));
                let dur = duration_ms as i64;
                sqlx::query(
                    "UPDATE agent_runs SET status = 'failed', input_tokens = 0, output_tokens = 0, duration_ms = $2, error = $3 WHERE id = $1",
                )
                .bind(&run_id)
                .bind(dur)
                .bind(err_msg.as_deref())
                .execute(&pool)
                .await
                .map_err(|e| HttpError::from(ares_types::types::AppError::Database(e.to_string())))?;
                run_guard.disarm();
                // Aggregate only after the parent exists; stays spawned, test polls.
                let obs_for_spawn = obs.clone();
                tokio::spawn(async move {
                    obs_for_spawn.aggregate_run_cost(dur).await;
                });
            }

            let response_agent_id = name.clone();
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
                config_source,
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
            last_used: None,
            expires_at: k.expires_at.map(ts_to_dt),
        })
        .collect();

    Ok(Json(response))
}

/// POST /v1/api-keys — create a new API key
pub async fn create_api_key(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Json(payload): Json<CreateApiKeyRequest>,
) -> Result<Json<CreateApiKeyResponse>> {
    let tc = extract_tenant(ctx)?;
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    let (api_key, raw_key) = state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
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
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Path(key_id): Path<String>,
) -> Result<StatusCode> {
    let tc = extract_tenant(ctx)?;
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    state_ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .revoke_api_key(&tc.tenant_id, &key_id)
        .await?;
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
        "note": "Tenant account retained. All operational data purged per GDPR Article 17."
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
        .route("/v1/agents/delete_tenant_data", delete(delete_tenant_data))
}

// cordis Phase6: RouteSet Service
use cordis::Service;
