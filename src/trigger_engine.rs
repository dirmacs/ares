//! Unified trigger execution engine.
//!
//! Executes agents in response to event triggers (webhook, document_upload,
//! field_change) with full observability, skill support, and pipeline
//! propagation.

use ares_cordis_core::Service;
use ares_db::agent_runs::{self, AgentRunMetadata};
use ares_db::schedules::EventTrigger;
use ares_types::types::AgentContext;
use std::sync::Arc;
use crate::AppState;

/// Cordis service stub for triggers — owns webhook/document-upload/field-change dispatch.
pub struct TriggerService;

impl Service for TriggerService {}

fn triggered_agent_run_metadata(
    trigger: &EventTrigger,
    run_id: &str,
    agent_config_source: &str,
    agent_config_version: Option<String>,
    eruka_context_hit: bool,
) -> AgentRunMetadata {
    AgentRunMetadata {
        workspace_id: None,
        session_id: Some(run_id.to_string()),
        request_source: Some("trigger".to_string()),
        product: None,
        agent_config_source: Some(agent_config_source.to_string()),
        agent_config_version,
        eruka_binding_id: None,
        eruka_context_hit,
        eruka_read_count: if eruka_context_hit { 1 } else { 0 },
        eruka_write_count: 0,
        pipeline_id: None,
        schedule_id: None,
        trigger_id: Some(trigger.id.clone()),
    }
}

/// Execute an agent in response to an event trigger.
///
/// This is the common pathway for webhook, document-upload, and field-change
/// triggers.  It resolves the agent, runs skill-based or regular execution,
/// records observability, and propagates to downstream pipelines.
pub async fn execute_triggered_agent(
    trigger: &EventTrigger,
    event_message: &str,
    app_state: &AppState,
) -> Result<(), String> {
    use crate::agents::Agent;
    use crate::agents::context_provider::AgentRuntimeContext;
    use crate::agents::tenant_agent;
    use crate::observability::{
        RunObservability, run_cost_aggregation_request, spawn_run_cost_aggregation,
    };

    let pool = app_state.get::<crate::context_services::TenantDbService>().expect("not provided").0.pool().clone();

    let tenant_agent_record =
        crate::db::tenant_agents::get_tenant_agent(&pool, &trigger.tenant_id, &trigger.target_agent)
            .await
            .map_err(|e| format!("Agent lookup failed: {}", e))?;

    let start = std::time::Instant::now();
    let run_id = uuid::Uuid::new_v4().to_string();

    // ── Skill-based execution ────────────────────────────────────────────
    // Skill-triggered agents bypass LLM provider resolution and need their
    // agent_runs row before skill steps write run_tool_calls.
    if let Some(skill_id) = tenant_agent_record
        .config
        .get("skill_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let metadata = triggered_agent_run_metadata(trigger, &run_id, "tenant_db", None, false);
        agent_runs::insert_agent_run_with_id_and_metadata(
            pool,
            &run_id,
            &trigger.tenant_id,
            &trigger.target_agent,
            None,
            "running",
            0,
            0,
            0,
            None,
            "skill",
            "skill",
            false,
            Some(&metadata),
        )
        .await
        .map_err(|e| e.to_string())?;

        app_state.get::<crate::context_services::ActiveRunsService>().expect("not provided").0.start(crate::active_runs::ActiveRun {
            run_id: run_id.clone(),
            tenant_id: trigger.tenant_id.clone(),
            agent_name: trigger.target_agent.clone(),
            started_at: chrono::Utc::now().timestamp(),
            status: "running".to_string(),
            current_step: 0,
            total_steps: 0,
            last_update: chrono::Utc::now().timestamp(),
            tool_name: Some(format!("skill:{skill_id}")),
            model: None,
            is_catchup: false,
            request_source: Some("trigger".to_string()),
            pipeline_id: None,
            schedule_id: None,
            trigger_id: Some(trigger.id.clone()),
        });

        let skill_result = app_state.get::<crate::context_services::SkillEngineService>().expect("not provided").0
            .execute_skill(
                skill_id,
                &trigger.tenant_id,
                serde_json::json!({"message": event_message}),
                &run_id,
            )
            .await;

        let duration_ms = start.elapsed().as_millis() as i64;
        let active_status = if skill_result.is_ok() {
            "completed"
        } else {
            "error"
        };
        app_state.get::<crate::context_services::ActiveRunsService>().expect("not provided").0.finish(&run_id, active_status);

        let status = if skill_result.is_ok() {
            "completed"
        } else {
            "failed"
        };
        let (input_tokens, output_tokens) = skill_result
            .as_ref()
            .map(crate::skill_engine::skill_result_token_counts)
            .unwrap_or((0, 0));
        let error_message = skill_result.as_ref().err().cloned();

        sqlx::query(
            "UPDATE agent_runs
             SET status = $2, input_tokens = $3, output_tokens = $4,
                 duration_ms = $5, error = $6
             WHERE id = $1",
        )
        .bind(&run_id)
        .bind(status)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(duration_ms)
        .bind(error_message.as_deref())
        .execute(&pool)
        .await
        .map_err(|e| e.to_string())?;

        spawn_run_cost_aggregation(
            pool.clone(),
            run_cost_aggregation_request(
                &run_id,
                &trigger.tenant_id,
                &trigger.target_agent,
                duration_ms,
            ),
        );

        let usage_pool = pool.clone();
        let usage_tid = trigger.tenant_id.clone();
        let usage_agent = trigger.target_agent.clone();
        let token_count = input_tokens + output_tokens;
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, 'trigger', $3, $4, $5, $6, $7, $8, $9, $10)"
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(usage_tid)
            .bind(1i32)
            .bind(token_count)
            .bind(input_tokens)
            .bind(output_tokens)
            .bind(Some("skill".to_string()))
            .bind(usage_agent)
            .bind(Some("skill".to_string()))
            .bind(chrono::Utc::now().timestamp())
            .execute(&usage_pool)
            .await;
        });

        if let Ok(val) = &skill_result {
            let output_str = serde_json::to_string(val).unwrap_or_default();
            let _ = crate::pipeline_engine::execute_pipeline_with_origin(
                &trigger.target_agent,
                &output_str,
                &trigger.tenant_id,
                Some(crate::pipeline_engine::PipelineOrigin::trigger(
                    trigger.id.clone(),
                )),
                app_state,
            )
            .await;
        }

        return skill_result.map(|_| ());
    }

    let mut resolved_agent = tenant_agent::resolve_agent_for_tenant(&pool,
        &app_state.get::<crate::context_services::AgentRegistryService>().expect("not provided").0,
        &trigger.tenant_id,
        &trigger.target_agent,
        &app_state.get::<crate::context_services::FleetSecretsService>().expect("not provided").0,
    )
    .await
    .map_err(|e| format!("Agent resolution failed: {}", e))?;

    // ── Regular agent execution ──────────────────────────────────────────
    let obs = Arc::new(RunObservability {
        run_id: run_id.clone(),
        tenant_id: trigger.tenant_id.clone(),
        agent_name: trigger.target_agent.clone(),
        pool: pool.clone(),
    });
    resolved_agent.agent.set_observability(obs.clone());
    resolved_agent.agent.set_runtime_tools(
        app_state.get::<crate::context_services::RuntimeToolRegistryService>().expect("not provided").0.clone(),
        trigger.tenant_id.clone(),
    );

    let mut runtime_context = AgentRuntimeContext::new(
        trigger.tenant_id.clone(),
        trigger.target_agent.clone(),
        "trigger",
    );
    runtime_context.session_id = Some(run_id.clone());

    let eruka_context = app_state.get::<crate::context_services::ContextProviderService>().expect("not provided").0
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

    let metadata = triggered_agent_run_metadata(
        trigger,
        &run_id,
        resolved_agent.source.as_str(),
        resolved_agent.config_version.clone(),
        eruka_context_hit,
    );

    agent_runs::insert_agent_run_with_id_and_metadata(
        pool,
        &run_id,
        &trigger.tenant_id,
        &trigger.target_agent,
        None,
        "running",
        0,
        0,
        0,
        None,
        "unknown",
        "unknown",
        false,
        Some(&metadata),
    )
    .await
    .map_err(|e| e.to_string())?;

    app_state.get::<crate::context_services::ActiveRunsService>().expect("not provided").0.start(crate::active_runs::ActiveRun {
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
        trigger_id: Some(trigger.id.clone()),
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
            app_state.get::<crate::context_services::ActiveRunsService>().expect("not provided").0
                .update_model(&run_id, Some(&model_name));
            app_state.get::<crate::context_services::ActiveRunsService>().expect("not provided").0.finish(&run_id, "completed");

            let _ = crate::pipeline_engine::execute_pipeline_with_origin(
                &trigger.target_agent,
                &response.content,
                &trigger.tenant_id,
                Some(crate::pipeline_engine::PipelineOrigin::trigger(
                    trigger.id.clone(),
                )),
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
            app_state.get::<crate::context_services::ActiveRunsService>().expect("not provided").0.finish(&run_id, "error");
        }
    }

    sqlx::query(
        "UPDATE agent_runs
         SET status = $2, input_tokens = $3, output_tokens = $4,
             duration_ms = $5, error = $6, model_name = $7, provider_name = $8
         WHERE id = $1",
    )
    .bind(&run_id)
    .bind(status)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(duration_ms as i64)
    .bind(error_msg.as_deref())
    .bind(&model_name)
    .bind(&provider_name)
    .execute(&pool)
    .await
    .map_err(|e| e.to_string())?;

    // Clone model/provider for usage event recording.
    let model_clone = model_name.clone();
    let provider_clone = provider_name.clone();

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

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger() -> EventTrigger {
        EventTrigger {
            id: "trigger-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            name: "on document".to_string(),
            event_type: "document_upload".to_string(),
            event_config: serde_json::json!({"bucket":"docs"}),
            target_agent: "agent-1".to_string(),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn triggered_agent_run_metadata_uses_trigger_id_not_pipeline_id() {
        let metadata = triggered_agent_run_metadata(
            &trigger(),
            "run-1",
            "tenant_db",
            Some("v1".to_string()),
            true,
        );

        assert_eq!(metadata.request_source.as_deref(), Some("trigger"));
        assert_eq!(metadata.trigger_id.as_deref(), Some("trigger-1"));
        assert_eq!(metadata.pipeline_id, None);
        assert_eq!(metadata.schedule_id, None);
        assert!(metadata.eruka_context_hit);
        assert_eq!(metadata.eruka_read_count, 1);
    }
}
