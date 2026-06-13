//! Unified trigger execution engine.
//!
//! Executes agents in response to event triggers (webhook, document_upload,
//! field_change) with full observability, skill support, and pipeline
//! propagation.

use ares_db::agent_runs::{self, AgentRunMetadata};
use ares_db::schedules::EventTrigger;
use ares_types::types::AgentContext;
use std::sync::Arc;

/// Execute an agent in response to an event trigger.
///
/// This is the common pathway for webhook, document-upload, and field-change
/// triggers.  It resolves the agent, runs skill-based or regular execution,
/// records observability, and propagates to downstream pipelines.
pub async fn execute_triggered_agent(
    trigger: &EventTrigger,
    event_message: &str,
    app_state: &Arc<crate::AppState>,
) -> Result<(), String> {
    use crate::agents::context_provider::AgentRuntimeContext;
    use crate::agents::tenant_agent;
    use crate::agents::Agent;
    use crate::observability::RunObservability;

    let pool = app_state.tenant_db.pool();

    let mut resolved_agent = tenant_agent::resolve_agent_for_tenant(
        pool,
        &app_state.agent_registry,
        &trigger.tenant_id,
        &trigger.target_agent,
        &app_state.fleet_secrets,
    )
    .await
    .map_err(|e| format!("Agent resolution failed: {}", e))?;

    let start = std::time::Instant::now();
    let run_id = uuid::Uuid::new_v4().to_string();

    // ── Skill-based execution ────────────────────────────────────────────
    if let Some(config) = &resolved_agent.config {
        if let Some(skill_id) = config.get("skill_id").and_then(|v| v.as_str()) {
            app_state.active_runs.start(crate::active_runs::ActiveRun {
                run_id: run_id.clone(),
                tenant_id: trigger.tenant_id.clone(),
                agent_name: trigger.target_agent.clone(),
                started_at: chrono::Utc::now().timestamp(),
                status: "running".to_string(),
                current_step: 0,
                total_steps: 0,
                last_update: chrono::Utc::now().timestamp(),
                tool_name: Some(format!("skill:{}", skill_id)),
                model: None,
                is_catchup: false,
                request_source: Some("trigger".to_string()),
                pipeline_id: None,
                schedule_id: None,
            });

            let skill_result = app_state
                .skill_engine
                .execute_skill(
                    skill_id,
                    &trigger.tenant_id,
                    serde_json::json!({"message": event_message}),
                    &run_id,
                )
                .await;

            let duration_ms = start.elapsed().as_millis() as u64;
            let skill_status = if skill_result.is_ok() {
                "completed"
            } else {
                "error"
            };
            app_state.active_runs.finish(&run_id, skill_status);

            let metadata = AgentRunMetadata {
                workspace_id: None,
                session_id: Some(run_id.clone()),
                request_source: Some("trigger".to_string()),
                product: None,
                agent_config_source: Some(resolved_agent.source.as_str().to_string()),
                agent_config_version: resolved_agent.config_version.clone(),
                eruka_binding_id: None,
                eruka_context_hit: false,
                eruka_read_count: 0,
                eruka_write_count: 0,
                pipeline_id: Some(trigger.id.clone()),
            };
            let status = if skill_result.is_ok() {
                "completed"
            } else {
                "failed"
            };
            let err_msg = skill_result.as_ref().err().cloned();

            let pool_clone = pool.clone();
            let tid = trigger.tenant_id.clone();
            let aname = trigger.target_agent.clone();
            let run_id_for_insert = run_id.clone();
            tokio::spawn(async move {
                let _ = agent_runs::insert_agent_run_with_id_and_metadata(
                    &pool_clone,
                    &run_id_for_insert,
                    &tid,
                    &aname,
                    None,
                    status,
                    0,
                    0,
                    duration_ms as i64,
                    err_msg.as_deref(),
                    "skill",
                    "skill",
                    false,
                    Some(&metadata),
                )
                .await;
            });

            let usage_pool = pool.clone();
            let usage_tid = trigger.tenant_id.clone();
            let usage_agent = trigger.target_agent.clone();
            tokio::spawn(async move {
                let _ = sqlx::query(
                    "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, 'trigger', $3, $4, $5, $6, $7, $8, $9, $10)"
                )
                .bind(uuid::Uuid::new_v4().to_string())
                .bind(usage_tid)
                .bind(1i32)
                .bind(0i64)
                .bind(0i64)
                .bind(0i64)
                .bind(Some("skill".to_string()))
                .bind(usage_agent)
                .bind(Some("skill".to_string()))
                .bind(chrono::Utc::now().timestamp())
                .execute(&usage_pool)
                .await;
            });

            if let Ok(val) = &skill_result {
                let output_str = serde_json::to_string(val).unwrap_or_default();
                let _ = crate::pipeline_engine::execute_pipeline(
                    &trigger.target_agent,
                    &output_str,
                    &trigger.tenant_id,
                    app_state,
                )
                .await;
            }

            return skill_result.map(|_| ()).map_err(|e| e);
        }
    }

    // ── Regular agent execution ──────────────────────────────────────────
    let obs = Arc::new(RunObservability {
        run_id: run_id.clone(),
        tenant_id: trigger.tenant_id.clone(),
        agent_name: trigger.target_agent.clone(),
        pool: pool.clone(),
    });
    resolved_agent.agent.set_observability(obs.clone());

    let mut runtime_context = AgentRuntimeContext::new(
        trigger.tenant_id.clone(),
        trigger.target_agent.clone(),
        "trigger",
    );
    runtime_context.session_id = Some(run_id.clone());

    let eruka_context = app_state
        .context_provider
        .get_context_for_run(&runtime_context)
        .await;
    let eruka_context_hit = eruka_context.is_some();
    let effective_message = if let Some(ctx) = eruka_context.as_deref() {
        crate::api::handlers::v1::format_message_with_context(ctx, event_message)
    } else {
        event_message.to_string()
    };

    let agent_context = AgentContext {
        user_id: trigger.tenant_id.clone(),
        session_id: run_id.clone(),
        conversation_history: vec![],
        user_memory: None,
    };

    app_state.active_runs.start(crate::active_runs::ActiveRun {
        run_id: run_id.clone(),
        tenant_id: trigger.tenant_id.clone(),
        agent_name: trigger.target_agent.clone(),
        started_at: chrono::Utc::now().timestamp(),
        status: "running".to_string(),
        current_step: 0,
        total_steps: 0,
        last_update: chrono::Utc::now().timestamp(),
        tool_name: None,
        model: None,
        is_catchup: false,
        request_source: Some("trigger".to_string()),
        pipeline_id: None,
        schedule_id: None,
    });

    let result = resolved_agent
        .agent
        .execute(&effective_message, &agent_context)
        .await;
    let duration_ms = start.elapsed().as_millis() as u64;

    let dur_i64 = duration_ms as i64;
    let obs_for_spawn = obs.clone();
    tokio::spawn(async move {
        obs_for_spawn.aggregate_run_cost(dur_i64).await;
    });

    let (status, error_msg, input_tokens, output_tokens, model_name, provider_name);

    match result {
        Ok(response) => {
            status = "completed";
            error_msg = None;
            let (itok, otok) = crate::api::handlers::v1::llm_token_counts_u64(
                response.usage.as_ref(),
                &effective_message,
                &response.content,
            );
            input_tokens = itok as i64;
            output_tokens = otok as i64;
            model_name = response
                .metadata
                .as_ref()
                .map(|m| m.model_name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            provider_name = response
                .metadata
                .as_ref()
                .map(|m| m.provider_name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            app_state
                .active_runs
                .update_model(&run_id, Some(&model_name));
            app_state.active_runs.finish(&run_id, "completed");

            let _ = crate::pipeline_engine::execute_pipeline(
                &trigger.target_agent,
                &response.content,
                &trigger.tenant_id,
                app_state,
            )
            .await;
        }
        Err(e) => {
            status = "failed";
            error_msg = Some(e.to_string());
            input_tokens = 0;
            output_tokens = 0;
            model_name = "unknown".to_string();
            provider_name = "unknown".to_string();
            app_state.active_runs.finish(&run_id, "error");
        }
    }

    let metadata = AgentRunMetadata {
        workspace_id: None,
        session_id: Some(run_id.clone()),
        request_source: Some("trigger".to_string()),
        product: None,
        agent_config_source: Some(resolved_agent.source.as_str().to_string()),
        agent_config_version: resolved_agent.config_version.clone(),
        eruka_binding_id: None,
        eruka_context_hit,
        eruka_read_count: if eruka_context_hit { 1 } else { 0 },
        eruka_write_count: 0,
        pipeline_id: Some(trigger.id.clone()),
    };

    let pool_clone = pool.clone();
    let tid = trigger.tenant_id.clone();
    let aname = trigger.target_agent.clone();
    let err_clone = error_msg.clone();
    let run_id_for_insert = run_id.clone();
    // Clone model/provider for usage event recording (both spawns need them)
    let model_clone = model_name.clone();
    let provider_clone = provider_name.clone();
    tokio::spawn(async move {
        let _ = agent_runs::insert_agent_run_with_id_and_metadata(
            &pool_clone,
            &run_id_for_insert,
            &tid,
            &aname,
            None,
            status,
            input_tokens,
            output_tokens,
            duration_ms as i64,
            err_clone.as_deref(),
            &model_name,
            &provider_name,
            false,
            Some(&metadata),
        )
        .await;
    });

    // Record usage event (fire-and-forget) - source='trigger'
    // SQL: INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at)
    //      VALUES ($1, $2, 'trigger', $3, $4, $5, $6, $7, $8, $9, $10)
    let usage_pool = pool.clone();
    let usage_tid = trigger.tenant_id.clone();
    let usage_model = if model_clone != "unknown" {
        Some(model_clone)
    } else {
        None
    };
    let usage_provider = if provider_clone != "unknown" {
        Some(provider_clone)
    } else {
        None
    };
    let usage_agent = trigger.target_agent.clone();
    let input_tok = input_tokens;
    let output_tok = output_tokens;
    let token_total = input_tokens + output_tokens;
    tokio::spawn(async move {
        let _ = sqlx::query(
            "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, 'trigger', $3, $4, $5, $6, $7, $8, $9, $10)"
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(usage_tid)
        .bind(1i32) // request_count
        .bind(token_total)
        .bind(input_tok)
        .bind(output_tok)
        .bind(usage_model)
        .bind(usage_agent)
        .bind(usage_provider)
        .bind(chrono::Utc::now().timestamp())
        .execute(&usage_pool)
        .await;
    });

    if let Some(err) = error_msg {
        return Err(format!("Agent execution failed: {}", err));
    }
    Ok(())
}
