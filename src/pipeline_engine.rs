//! Inter-agent pipeline execution engine.

use ares_db::agent_runs::{self, AgentRunMetadata};
use ares_db::schedules::{AgentPipeline, PipelineStore};
use ares_types::types::AgentContext;
use std::sync::Arc;

/// Execute all enabled pipelines originating from `source_agent_name`, passing
/// `source_output` as input to downstream agents. Returns the list of target
/// agent names that were successfully triggered.
pub async fn execute_pipeline(
    source_agent_name: &str,
    source_output: &str,
    tenant_id: &str,
    app_state: &Arc<crate::AppState>,
) -> Result<Vec<String>, String> {
    let store = PipelineStore::new(app_state.tenant_db.pool());
    let pipelines = store
        .get_pipelines_for_source(tenant_id, source_agent_name)
        .await
        .map_err(|e| e.to_string())?;

    let mut triggered = Vec::new();
    for pipeline in pipelines {
        if let Some(condition) = &pipeline.condition {
            if !evaluate_condition(condition, source_output) {
                continue;
            }
        }

        tracing::info!(
            "Executing pipeline: {} -> {} (tenant {})",
            source_agent_name,
            pipeline.target_agent,
            tenant_id
        );

        match execute_target_agent(&pipeline, source_output, tenant_id, app_state).await {
            Ok(_) => triggered.push(pipeline.target_agent.clone()),
            Err(e) => tracing::error!(
                "Pipeline target {} failed for tenant {}: {}",
                pipeline.target_agent,
                tenant_id,
                e
            ),
        }
    }
    Ok(triggered)
}

async fn execute_target_agent(
    pipeline: &AgentPipeline,
    source_output: &str,
    tenant_id: &str,
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
        tenant_id,
        &pipeline.target_agent,
        &app_state.fleet_secrets,
    )
    .await
    .map_err(|e| format!("Agent resolution failed: {}", e))?;

    let start = std::time::Instant::now();
    let run_id = uuid::Uuid::new_v4().to_string();

    // Skill-based execution
    if let Some(config) = &resolved_agent.config {
        if let Some(skill_id) = config.get("skill_id").and_then(|v| v.as_str()) {
            app_state.active_runs.start(crate::active_runs::ActiveRun {
                run_id: run_id.clone(),
                tenant_id: tenant_id.to_string(),
                agent_name: pipeline.target_agent.clone(),
                started_at: chrono::Utc::now().timestamp(),
                status: "running".to_string(),
                current_step: 0,
                total_steps: 0,
                last_update: chrono::Utc::now().timestamp(),
                tool_name: Some(format!("skill:{}", skill_id)),
                model: None,
            });

            let skill_result = app_state
                .skill_engine
                .execute_skill(
                    skill_id,
                    tenant_id,
                    serde_json::json!({"message": source_output}),
                    &run_id,
                )
                .await;

            let duration_ms = start.elapsed().as_millis() as u64;
            let skill_status = if skill_result.is_ok() { "completed" } else { "error" };
            app_state.active_runs.finish(&run_id, skill_status);

            let metadata = AgentRunMetadata {
                workspace_id: None,
                session_id: Some(run_id.clone()),
                request_source: Some("pipeline".to_string()),
                product: None,
                agent_config_source: Some(resolved_agent.source.as_str().to_string()),
                agent_config_version: resolved_agent.config_version.clone(),
                eruka_binding_id: None,
                eruka_context_hit: false,
                eruka_read_count: 0,
                eruka_write_count: 0,
                pipeline_id: Some(pipeline.id.clone()),
            };
            let status = if skill_result.is_ok() { "completed" } else { "failed" };
            let err_msg = skill_result.as_ref().err().cloned();

            let pool_clone = pool.clone();
            let tid = tenant_id.to_string();
            let aname = pipeline.target_agent.clone();
            tokio::spawn(async move {
                let _ = agent_runs::insert_agent_run_with_metadata(
                    &pool_clone,
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

            return skill_result.map(|_| ()).map_err(|e| e);
        }
    }

    // Regular agent execution
    let obs = Arc::new(RunObservability {
        run_id: run_id.clone(),
        tenant_id: tenant_id.to_string(),
        agent_name: pipeline.target_agent.clone(),
        pool: pool.clone(),
    });
    resolved_agent.agent.set_observability(obs.clone());

    let mut runtime_context = AgentRuntimeContext::new(
        tenant_id.to_string(),
        pipeline.target_agent.clone(),
        "pipeline",
    );
    runtime_context.session_id = Some(run_id.clone());

    let eruka_context = app_state
        .context_provider
        .get_context_for_run(&runtime_context)
        .await;
    let eruka_context_hit = eruka_context.is_some();
    let effective_message = if let Some(ctx) = eruka_context.as_deref() {
        crate::api::handlers::v1::format_message_with_context(ctx, source_output)
    } else {
        source_output.to_string()
    };

    let agent_context = AgentContext {
        user_id: tenant_id.to_string(),
        session_id: run_id.clone(),
        conversation_history: vec![],
        user_memory: None,
    };

    app_state.active_runs.start(crate::active_runs::ActiveRun {
        run_id: run_id.clone(),
        tenant_id: tenant_id.to_string(),
        agent_name: pipeline.target_agent.clone(),
        started_at: chrono::Utc::now().timestamp(),
        status: "running".to_string(),
        current_step: 0,
        total_steps: 0,
        last_update: chrono::Utc::now().timestamp(),
        tool_name: None,
        model: None,
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
            app_state.active_runs.update_model(&run_id, Some(&model_name));
            app_state.active_runs.finish(&run_id, "completed");
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
        request_source: Some("pipeline".to_string()),
        product: None,
        agent_config_source: Some(resolved_agent.source.as_str().to_string()),
        agent_config_version: resolved_agent.config_version.clone(),
        eruka_binding_id: None,
        eruka_context_hit,
        eruka_read_count: if eruka_context_hit { 1 } else { 0 },
        eruka_write_count: 0,
        pipeline_id: Some(pipeline.id.clone()),
    };

    let pool_clone = pool.clone();
    let tid = tenant_id.to_string();
    let aname = pipeline.target_agent.clone();
    let err_clone = error_msg.clone();
    tokio::spawn(async move {
        let _ = agent_runs::insert_agent_run_with_metadata(
            &pool_clone,
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

    if let Some(err) = error_msg {
        return Err(format!("Agent execution failed: {}", err));
    }
    Ok(())
}

/// Evaluate a simple string condition against an agent output.
///
/// Supported syntax:
/// - `output.contains("X")`
/// - `output.starts_with("X")`
/// - `output.ends_with("X")`
/// - `output == "X"`
/// - `output != "X"`
///
/// Falls back to a plain substring check if the expression does not match any
/// of the above patterns.
pub fn evaluate_condition(condition: &str, output: &str) -> bool {
    let condition = condition.trim();
    if condition.is_empty() {
        return true;
    }

    if let Some(inner) = condition.strip_prefix("output.contains(\"") {
        if let Some(val) = inner.strip_suffix("\")") {
            return output.contains(val);
        }
    }
    if let Some(inner) = condition.strip_prefix("output.starts_with(\"") {
        if let Some(val) = inner.strip_suffix("\")") {
            return output.starts_with(val);
        }
    }
    if let Some(inner) = condition.strip_prefix("output.ends_with(\"") {
        if let Some(val) = inner.strip_suffix("\")") {
            return output.ends_with(val);
        }
    }
    if let Some(inner) = condition.strip_prefix("output == \"") {
        if let Some(val) = inner.strip_suffix("\"") {
            return output == val;
        }
    }
    if let Some(inner) = condition.strip_prefix("output != \"") {
        if let Some(val) = inner.strip_suffix("\"") {
            return output != val;
        }
    }

    // Fallback: treat as simple substring check
    output.contains(condition)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluate_condition() {
        assert!(evaluate_condition("output.contains(\"hello\")", "hello world"));
        assert!(!evaluate_condition("output.contains(\"xyz\")", "hello world"));
        assert!(evaluate_condition("output.starts_with(\"hello\")", "hello world"));
        assert!(evaluate_condition("output.ends_with(\"world\")", "hello world"));
        assert!(evaluate_condition("output == \"hello\"", "hello"));
        assert!(!evaluate_condition("output == \"hello\"", "hello world"));
        assert!(evaluate_condition("output != \"foo\"", "hello"));
        assert!(evaluate_condition("hello", "hello world")); // fallback
    }

    #[test]
    fn test_evaluate_condition_empty() {
        assert!(evaluate_condition("", "anything"));
    }

    #[test]
    fn test_evaluate_condition_json_output() {
        let json = r#"{"status":"success","result":"completed"}"#;
        assert!(evaluate_condition("output.contains(\"success\")", json));
        assert!(!evaluate_condition("output.contains(\"failure\")", json));
        assert!(evaluate_condition("output.starts_with(\"{\")", json));
        assert!(evaluate_condition("output.ends_with(\"}\")", json));
        assert!(evaluate_condition("output != \"\"", json));
    }
}