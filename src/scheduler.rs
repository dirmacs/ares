//! Background cron scheduler for agent runs.
//!
//! Periodically checks `agent_schedules` table for agents whose `next_run_at`
//! is in the past, runs them, and updates `last_run_at` / `next_run_at`.

use ares_db::agent_runs::{self, AgentRunMetadata};
use ares_db::schedules::{compute_next_run, AgentSchedule, MissedRunAudit, ScheduleStore};
use ares_types::types::AgentContext;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScheduledPipelineTrigger<'a> {
    pub(crate) source_agent: &'a str,
    pub(crate) source_output: &'a str,
    pub(crate) tenant_id: &'a str,
}

pub(crate) fn scheduled_pipeline_trigger<'a>(
    sched: &'a AgentSchedule,
    source_output: &'a str,
) -> ScheduledPipelineTrigger<'a> {
    ScheduledPipelineTrigger {
        source_agent: &sched.agent_name,
        source_output,
        tenant_id: &sched.tenant_id,
    }
}

/// Start the background scheduler loop.
pub async fn start_scheduler(pool: PgPool, app_state: Arc<crate::AppState>) {
    let mut ticker = interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        if let Err(e) = run_due_schedules(&pool, &app_state).await {
            tracing::warn!("Scheduler tick failed: {}", e);
        }
    }
}

async fn run_due_schedules(pool: &PgPool, app_state: &Arc<crate::AppState>) -> Result<(), String> {
    let store = ScheduleStore::new(pool);

    // 1. First, handle catchup for missed runs within grace period
    if let Err(e) = run_catchup_schedules(&store, app_state).await {
        tracing::warn!("Scheduler catchup failed: {}", e);
    }

    // 2. Then, run normally scheduled agents
    let due = store.get_due_schedules().await.map_err(|e| e.to_string())?;
    for sched in due {
        tracing::info!(
            "Scheduler: running agent {} for tenant {}",
            sched.agent_name,
            sched.tenant_id
        );
        if let Err(e) = execute_scheduled_agent(&sched, app_state, false).await {
            tracing::warn!(
                "Scheduled run failed for agent {} (tenant {}): {}",
                sched.agent_name,
                sched.tenant_id,
                e
            );
        }
        match compute_next_run(&sched.cron_expression, &sched.timezone) {
            Ok(next) => {
                if let Err(e) = store.update_schedule_run(&sched.id, next).await {
                    tracing::warn!("Failed to update schedule {} next_run_at: {}", sched.id, e);
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to compute next run for schedule {}: {}",
                    sched.id,
                    e
                );
            }
        }
    }
    Ok(())
}

/// Detect schedules whose `next_run_at` is past their grace window and trigger
/// a single catch-up run. Records each catch-up in the `missed_runs` audit
/// table so we never trigger the same missed slot twice.
async fn run_catchup_schedules(
    store: &ScheduleStore<'_>,
    app_state: &Arc<crate::AppState>,
) -> Result<(), String> {
    let overdue = store
        .get_overdue_for_catchup()
        .await
        .map_err(|e| e.to_string())?;

    if overdue.is_empty() {
        return Ok(());
    }

    let now = chrono::Utc::now().timestamp();

    for sched in overdue {
        // Already caught up since last_run_at >= next_run_at? Skip.
        if let (Some(last), Some(next)) = (sched.last_run_at, sched.next_run_at) {
            if last >= next {
                continue;
            }
        }

        let expected_at = sched.next_run_at.unwrap_or(now);
        tracing::info!(
            "Scheduler catchup: scheduling {} for tenant {} (was due at {}, grace={}s)",
            sched.agent_name,
            sched.tenant_id,
            expected_at,
            sched.grace_period_seconds,
        );

        let audit = MissedRunAudit {
            id: uuid::Uuid::new_v4().to_string(),
            schedule_id: sched.id.clone(),
            expected_at,
            detected_at: now,
            action_taken: "catchup_triggered".to_string(),
            created_at: now,
        };
        if let Err(e) = store.insert_missed_run_audit(&audit).await {
            tracing::warn!("Failed to record missed_run audit for {}: {}", sched.id, e);
        }

        if let Err(e) = execute_scheduled_agent(&sched, app_state, true).await {
            tracing::warn!(
                "Catchup run failed for agent {} (tenant {}): {}",
                sched.agent_name,
                sched.tenant_id,
                e
            );
        } else {
            // Mark schedule as caught up and compute next future run time
            match compute_next_run(&sched.cron_expression, &sched.timezone) {
                Ok(next) => {
                    if let Err(e) = store.update_schedule_run(&sched.id, next).await {
                        tracing::warn!(
                            "Failed to update schedule {} after catchup: {}",
                            sched.id,
                            e
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to compute next run for schedule {} after catchup: {}",
                        sched.id,
                        e
                    );
                }
            }
        }
    }

    Ok(())
}

async fn execute_scheduled_agent(
    sched: &AgentSchedule,
    app_state: &Arc<crate::AppState>,
    is_catchup: bool,
) -> Result<(), String> {
    use crate::agents::context_provider::AgentRuntimeContext;
    use crate::agents::tenant_agent;
    use crate::agents::Agent;
    use crate::observability::RunObservability;

    let pool = app_state.tenant_db.pool();

    // 1. Resolve agent
    let mut resolved_agent = match tenant_agent::resolve_agent_for_tenant(
        pool,
        &app_state.agent_registry,
        &sched.tenant_id,
        &sched.agent_name,
        &app_state.fleet_secrets,
    )
    .await
    {
        Ok(agent) => agent,
        Err(e) => {
            tracing::error!(
                "Failed to resolve agent {} for tenant {}: {}",
                sched.agent_name,
                sched.tenant_id,
                e
            );
            return Err(format!("Agent resolution failed: {}", e));
        }
    };

    let start = std::time::Instant::now();
    let run_id = uuid::Uuid::new_v4().to_string();

    // 2. Skill-based agent execution
    if let Some(config) = &resolved_agent.config {
        if let Some(skill_id) = config.get("skill_id").and_then(|v| v.as_str()) {
            app_state.active_runs.start(crate::active_runs::ActiveRun {
                run_id: run_id.clone(),
                tenant_id: sched.tenant_id.clone(),
                agent_name: sched.agent_name.clone(),
                started_at: chrono::Utc::now().timestamp(),
                status: "running".to_string(),
                current_step: 0,
                total_steps: 0,
                last_update: chrono::Utc::now().timestamp(),
                tool_name: Some(format!("skill:{}", skill_id)),
                model: None,
                is_catchup,
            });

            let skill_result = app_state
                .skill_engine
                .execute_skill(
                    skill_id,
                    &sched.tenant_id,
                    serde_json::json!({"message": "scheduled run"}),
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

            if let Ok(val) = &skill_result {
                let output_str = serde_json::to_string(val).unwrap_or_default();
                let trigger = scheduled_pipeline_trigger(sched, &output_str);
                let _ = crate::pipeline_engine::execute_pipeline(
                    trigger.source_agent,
                    trigger.source_output,
                    trigger.tenant_id,
                    app_state,
                )
                .await;
            }

            // Record agent run
            let metadata = AgentRunMetadata {
                workspace_id: None,
                session_id: Some(run_id.clone()),
                request_source: Some("scheduled".to_string()),
                product: None,
                agent_config_source: Some(resolved_agent.source.as_str().to_string()),
                agent_config_version: resolved_agent.config_version.clone(),
                eruka_binding_id: None,
                eruka_context_hit: false,
                eruka_read_count: 0,
                eruka_write_count: 0,
                pipeline_id: None,
            };
            let status = if skill_result.is_ok() {
                "completed"
            } else {
                "failed"
            };
            let err_msg = skill_result.as_ref().err().cloned();

            let pool_clone = pool.clone();
            let tid = sched.tenant_id.clone();
            let aname = sched.agent_name.clone();
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
            let usage_tid = sched.tenant_id.clone();
            let usage_agent = sched.agent_name.clone();
            tokio::spawn(async move {
                let _ = sqlx::query(
                    "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, 'scheduled', $3, $4, $5, $6, $7, $8, $9, $10)"
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

            return skill_result.map(|_| ()).map_err(|e| e);
        }
    }

    // 3. Regular agent execution
    let obs = Arc::new(RunObservability {
        run_id: run_id.clone(),
        tenant_id: sched.tenant_id.clone(),
        agent_name: sched.agent_name.clone(),
        pool: pool.clone(),
    });
    resolved_agent.agent.set_observability(obs.clone());

    let mut runtime_context = AgentRuntimeContext::new(
        sched.tenant_id.clone(),
        sched.agent_name.clone(),
        "scheduled",
    );
    runtime_context.session_id = Some(run_id.clone());

    let eruka_context = app_state
        .context_provider
        .get_context_for_run(&runtime_context)
        .await;
    let eruka_context_hit = eruka_context.is_some();
    let effective_message = if let Some(ctx) = eruka_context.as_deref() {
        tracing::info!(
            agent = %sched.agent_name,
            tenant = %sched.tenant_id,
            ctx_len = ctx.len(),
            "External context injected into scheduled agent run"
        );
        crate::api::handlers::v1::format_message_with_context(ctx, "scheduled run")
    } else {
        "scheduled run".to_string()
    };

    let agent_context = AgentContext {
        user_id: sched.tenant_id.clone(),
        session_id: run_id.clone(),
        conversation_history: vec![],
        user_memory: None,
    };

    app_state.active_runs.start(crate::active_runs::ActiveRun {
        run_id: run_id.clone(),
        tenant_id: sched.tenant_id.clone(),
        agent_name: sched.agent_name.clone(),
        started_at: chrono::Utc::now().timestamp(),
        status: "running".to_string(),
        current_step: 0,
        total_steps: 0,
        last_update: chrono::Utc::now().timestamp(),
        tool_name: None,
        model: None,
        is_catchup,
    });

    let result = resolved_agent
        .agent
        .execute(&effective_message, &agent_context)
        .await;
    let duration_ms = start.elapsed().as_millis() as u64;

    // Aggregate run costs (fire-and-forget)
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

            let trigger = scheduled_pipeline_trigger(sched, &response.content);
            let _ = crate::pipeline_engine::execute_pipeline(
                trigger.source_agent,
                trigger.source_output,
                trigger.tenant_id,
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

    // Record agent run (fire-and-forget)
    let metadata = AgentRunMetadata {
        workspace_id: None,
        session_id: Some(run_id.clone()),
        request_source: Some("scheduled".to_string()),
        product: None,
        agent_config_source: Some(resolved_agent.source.as_str().to_string()),
        agent_config_version: resolved_agent.config_version.clone(),
        eruka_binding_id: None,
        eruka_context_hit,
        eruka_read_count: if eruka_context_hit { 1 } else { 0 },
        eruka_write_count: 0,
        pipeline_id: None,
    };

    let _has_error = error_msg.is_some();
    let pool_clone = pool.clone();
    let tid = sched.tenant_id.clone();
    let aname = sched.agent_name.clone();
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

    // Record usage event (fire-and-forget) - source='scheduled'
    // SQL: INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at)
    //      VALUES ($1, $2, 'scheduled', $3, $4, $5, $6, $7, $8, $9, $10)
    let usage_pool = pool.clone();
    let usage_tid = sched.tenant_id.clone();
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
    let usage_agent = sched.agent_name.clone();
    let input_tok = input_tokens;
    let output_tok = output_tokens;
    let token_total = input_tokens + output_tokens;
    tokio::spawn(async move {
        let _ = sqlx::query(
            "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, 'scheduled', $3, $4, $5, $6, $7, $8, $9, $10)"
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
    use ares_db::schedules::AgentPipeline;

    #[test]
    fn compute_next_run_with_valid_cron() {
        // Every minute — next run should be within the next 60 seconds.
        let next = compute_next_run("* * * * * *", "UTC").expect("valid cron");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            next > now && next <= now + 60,
            "next={} should be within 60s of now={}",
            next,
            now
        );
    }

    #[test]
    fn compute_next_run_with_invalid_cron() {
        let result = compute_next_run("not-a-cron", "UTC");
        assert!(result.is_err(), "invalid cron should return Err");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("Invalid cron expression"),
            "error should mention invalid cron: {}",
            msg
        );
    }

    #[test]
    fn scheduled_agent_success_prepares_downstream_pipeline_execution_and_billing() {
        let schedule = AgentSchedule {
            id: "schedule-1".to_string(),
            tenant_id: "tenant-a".to_string(),
            agent_name: "source-agent".to_string(),
            cron_expression: "* * * * * *".to_string(),
            timezone: "UTC".to_string(),
            enabled: true,
            last_run_at: None,
            next_run_at: Some(1_700_000_000),
            grace_period_seconds: 120,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
        };
        let source_output = r#"{"status":"success","result":"ready"}"#;
        let trigger = scheduled_pipeline_trigger(&schedule, source_output);
        let pipeline = AgentPipeline {
            id: "pipeline-source-to-target".to_string(),
            tenant_id: trigger.tenant_id.to_string(),
            source_agent: trigger.source_agent.to_string(),
            target_agent: "target-agent".to_string(),
            condition: Some("output.contains(\"success\")".to_string()),
            enabled: true,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
        };

        assert_eq!(trigger.source_agent, "source-agent");
        assert_eq!(trigger.tenant_id, "tenant-a");
        assert!(crate::pipeline_engine::evaluate_condition(
            pipeline.condition.as_deref().unwrap(),
            trigger.source_output
        ));
        assert_eq!(pipeline.source_agent, schedule.agent_name);

        let effects = crate::pipeline_engine::pipeline_target_run_effects(
            &pipeline,
            trigger.tenant_id,
            "target-run-1",
            Some("tenant-db"),
            Some("cfg-v1".to_string()),
            true,
            17,
            23,
            "gpt-4o-mini",
            "openai",
        );

        assert_eq!(effects.metadata.request_source.as_deref(), Some("pipeline"));
        assert_eq!(
            effects.metadata.pipeline_id.as_deref(),
            Some("pipeline-source-to-target")
        );
        assert_eq!(effects.metadata.session_id.as_deref(), Some("target-run-1"));
        assert!(effects.metadata.eruka_context_hit);
        assert_eq!(effects.metadata.eruka_read_count, 1);
        assert_eq!(effects.usage.tenant_id, "tenant-a");
        assert_eq!(effects.usage.source, "pipeline");
        assert_eq!(effects.usage.request_count, 1);
        assert_eq!(effects.usage.agent_name, "target-agent");
        assert_eq!(effects.usage.input_tokens, 17);
        assert_eq!(effects.usage.output_tokens, 23);
        assert_eq!(effects.usage.token_count, 40);
        assert_eq!(effects.usage.model_name.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(effects.usage.provider_name.as_deref(), Some("openai"));
    }

    #[test]
    fn scheduled_cron_every_minute_within_60s() {
        let next = compute_next_run("* * * * * *", "UTC").expect("valid cron");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            next > now && next <= now + 60,
            "next={} should be within 60s of now={}",
            next,
            now
        );
    }

    #[test]
    fn scheduled_cron_daily_midnight_within_25h() {
        let next = compute_next_run("0 0 0 * * *", "UTC").expect("valid standard cron");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(
            next > now && next <= now + 25 * 3600,
            "next={} should be within 25h of now={}",
            next,
            now
        );
    }
}
