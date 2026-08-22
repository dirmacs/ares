//! Inter-agent pipeline execution engine.

use ares_cordis_core::{Context, CordisError, Disposable, Service};
use ares_db::agent_runs::{self, AgentRunMetadata};
use ares_db::schedules::{AgentPipeline, PipelineStore};
use ares_db::PostgresClient;
use ares_agents::execution::{AgentExecutionService, AgentRequest};
use ares_types::types::AgentContext;
use serde_json::Value;
use std::sync::Arc;
use tokio::task::JoinHandle;
use crate::AppState;

/// Cordis service owning `agent_pipelines` lookup and conditional evaluation,
/// injecting `AgentExecutionService` for downstream execution.
///
/// Pipelines are triggered downstream (no tick loop); `Service::init` merely
/// proves `ReflectService` wiring and returns no guard (pipelines are
/// event-driven via `execute_pipeline` / `execute_pipeline_with_origin`).
pub struct PipelineService {
    pub db: Arc<PostgresClient>,
    pub execution: Arc<AgentExecutionService>,
    _handle: parking_lot::Mutex<Option<JoinHandle<()>>>,
}

impl PipelineService {
    /// Create a new service with explicit dependencies.
    pub fn new(db: Arc<PostgresClient>, execution: Arc<AgentExecutionService>) -> Self {
        Self {
            db,
            execution,
            _handle: parking_lot::Mutex::new(None),
        }
    }

    /// Execute a single pipeline by id for a tenant, applying conditional
    /// evaluation against `input` and then delegating to
    /// `ctx.get::<AgentExecutionService>().execute` (fallback: `self.execution`).
    ///
    /// Lookup: `agent_pipelines` where `tenant_id=$1 AND id=$2` (enabled check
    /// enforced — disabled pipelines return `Err`).
    pub async fn execute_pipeline(
        &self,
        pipeline_id: &str,
        tenant: &str,
        input: Value,
        ctx: &Arc<Context>,
    ) -> Result<Value, String> {
        let pool = &self.db.pool;
        // Direct lookup by id+tenant (no dedicated store method — use raw query
        // to avoid adding a new store API while still proving `agent_pipelines` ownership).
        let row = sqlx::query(
            "SELECT id, tenant_id, source_agent, target_agent, condition, enabled, created_at, updated_at \
             FROM agent_pipelines WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant)
        .bind(pipeline_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("pipeline {pipeline_id} not found for tenant {tenant}"))?;

        use sqlx::Row;
        let pipeline = AgentPipeline {
            id: row.try_get::<String, _>("id").map_err(|e| e.to_string())?,
            tenant_id: row.try_get::<String, _>("tenant_id").map_err(|e| e.to_string())?,
            source_agent: row.try_get::<String, _>("source_agent").map_err(|e| e.to_string())?,
            target_agent: row.try_get::<String, _>("target_agent").map_err(|e| e.to_string())?,
            condition: row.try_get::<Option<String>, _>("condition").map_err(|e| e.to_string())?,
            enabled: row.try_get::<bool, _>("enabled").map_err(|e| e.to_string())?,
            created_at: row.try_get::<i64, _>("created_at").map_err(|e| e.to_string())?,
            updated_at: row.try_get::<i64, _>("updated_at").map_err(|e| e.to_string())?,
        };

        if !pipeline.enabled {
            return Err(format!("pipeline {pipeline_id} is disabled"));
        }

        let input_str = match &input {
            Value::String(s) => s.clone(),
            _ => input.to_string(),
        };

        if let Some(condition) = &pipeline.condition {
            if !evaluate_condition(condition, &input_str) {
                return Err(format!(
                    "pipeline {pipeline_id} condition not met: {condition}"
                ));
            }
        }

        // Prefer Context-provided execution (DI), fallback to injected `self.execution`.
        let exec: Arc<AgentExecutionService> = ctx
            .get::<AgentExecutionService>()
            .unwrap_or_else(|| self.execution.clone());

        let req = AgentRequest {
            agent_name: pipeline.target_agent.clone(),
            tenant: Some(tenant.to_string()),
            message: input_str,
            history: Vec::new(),
            ctx_provider: None,
        };

        // Isolate wins over intercept over fallback; keep req.tenant for execute fallback.
        let scoped = tenant_scoped_ctx(ctx, tenant);
        let resp = match exec.execute_agent(&req, &scoped).await {
            Ok(result) => result.response,
            Err(_) => exec.execute(req, &scoped).await.map_err(|e| e.to_string())?,
        };

        // Surface as JSON Value for caller uniformity.
        Ok(serde_json::json!({
            "pipeline_id": pipeline.id,
            "target_agent": pipeline.target_agent,
            "content": resp.content,
            "usage": resp.usage,
        }))
    }
}

// Guard that aborts background task on dispose (kept for symmetry with SchedulerService).
struct PipelineGuard {
    handle: Arc<parking_lot::Mutex<Option<JoinHandle<()>>>>,
}

impl Disposable for PipelineGuard {
    fn dispose(self: Box<Self>) {
        if let Some(h) = self.handle.lock().take() {
            h.abort();
        }
    }
}

impl Service for PipelineService {
    fn name(&self) -> &'static str {
        "PipelineService"
    }

    fn init(&self, ctx: &Arc<Context>) -> ares_cordis_core::ServiceInitFuture<'_> {
        // Prove ReflectService wiring (no tick loop needed — pipelines are downstream-triggered).
        // Mirror SchedulerService's ensure_notifier/register_dependent pattern so wiring is uniform.
        if let Some(reflect) = ctx.get::<ares_cordis_core::ReflectService>() {
            use std::any::TypeId;
            let tid = TypeId::of::<PipelineService>();
            let _rx = reflect.ensure_notifier(tid);
            reflect.register_dependent(tid, 1);
            reflect.set_context(ctx);
        }
        // No background loop; return None (no guard needed) but keep handle slot for lifecycle symmetry.
        Box::pin(async move { Ok(None) })
    }
}

pub(crate) const PIPELINE_REQUEST_SOURCE: &str = "pipeline";

pub(crate) fn tenant_scoped_ctx(ctx: &Arc<Context>, tenant_id: &str) -> Arc<Context> {
    ctx.isolate::<ares_agents::AgentResolverService>(&format!("tenant:{tenant_id}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PipelineUsageRecord {
    pub(crate) tenant_id: String,
    pub(crate) source: &'static str,
    pub(crate) request_count: i32,
    pub(crate) token_count: i64,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) model_name: Option<String>,
    pub(crate) agent_name: String,
    pub(crate) provider_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PipelineTargetRunEffects {
    pub(crate) metadata: AgentRunMetadata,
    pub(crate) usage: PipelineUsageRecord,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PipelineOrigin {
    pub(crate) is_catchup: bool,
    pub(crate) schedule_id: Option<String>,
    pub(crate) trigger_id: Option<String>,
}

impl PipelineOrigin {
    pub(crate) fn scheduled(schedule_id: String, is_catchup: bool) -> Self {
        Self {
            is_catchup,
            schedule_id: Some(schedule_id),
            trigger_id: None,
        }
    }

    pub(crate) fn trigger(trigger_id: String) -> Self {
        Self {
            is_catchup: false,
            schedule_id: None,
            trigger_id: Some(trigger_id),
        }
    }
}

pub(crate) fn pipeline_active_run(
    run_id: &str,
    tenant_id: &str,
    agent_name: &str,
    pipeline_id: &str,
    origin: Option<&PipelineOrigin>,
    tool_name: Option<String>,
) -> crate::active_runs::ActiveRun {
    let now = chrono::Utc::now().timestamp();
    crate::active_runs::ActiveRun {
        run_id: run_id.to_string(),
        tenant_id: tenant_id.to_string(),
        agent_name: agent_name.to_string(),
        started_at: now,
        status: "running".to_string(),
        current_step: 0,
        total_steps: 0,
        last_update: now,
        tool_name,
        model: None,
        is_catchup: origin.map(|origin| origin.is_catchup).unwrap_or(false),
        request_source: Some(PIPELINE_REQUEST_SOURCE.to_string()),
        pipeline_id: Some(pipeline_id.to_string()),
        schedule_id: origin.and_then(|origin| origin.schedule_id.clone()),
        trigger_id: origin.and_then(|origin| origin.trigger_id.clone()),
    }
}

pub(crate) fn pipeline_target_run_effects(
    pipeline: &AgentPipeline,
    tenant_id: &str,
    run_id: &str,
    origin: Option<&PipelineOrigin>,
    agent_config_source: Option<&str>,
    agent_config_version: Option<String>,
    eruka_context_hit: bool,
    input_tokens: i64,
    output_tokens: i64,
    model_name: &str,
    provider_name: &str,
) -> PipelineTargetRunEffects {
    PipelineTargetRunEffects {
        metadata: AgentRunMetadata {
            workspace_id: None,
            session_id: Some(run_id.to_string()),
            request_source: Some(PIPELINE_REQUEST_SOURCE.to_string()),
            product: None,
            agent_config_source: agent_config_source.map(str::to_string),
            agent_config_version,
            eruka_binding_id: None,
            eruka_context_hit,
            eruka_read_count: if eruka_context_hit { 1 } else { 0 },
            eruka_write_count: 0,
            pipeline_id: Some(pipeline.id.clone()),
            schedule_id: origin.and_then(|origin| origin.schedule_id.clone()),
            trigger_id: origin.and_then(|origin| origin.trigger_id.clone()),
        },
        usage: PipelineUsageRecord {
            tenant_id: tenant_id.to_string(),
            source: PIPELINE_REQUEST_SOURCE,
            request_count: 1,
            token_count: input_tokens + output_tokens,
            input_tokens,
            output_tokens,
            model_name: (model_name != "unknown").then(|| model_name.to_string()),
            agent_name: pipeline.target_agent.clone(),
            provider_name: (provider_name != "unknown").then(|| provider_name.to_string()),
        },
    }
}

/// Execute all enabled pipelines originating from `source_agent_name`, passing
/// `source_output` as input to downstream agents. Returns the list of target
/// agent names that were successfully triggered.
pub async fn execute_pipeline(
    source_agent_name: &str,
    source_output: &str,
    tenant_id: &str,
    app_state: &AppState,
) -> Result<Vec<String>, String> {
    execute_pipeline_with_origin(source_agent_name, source_output, tenant_id, None, app_state).await
}

pub(crate) async fn execute_pipeline_with_origin(
    source_agent_name: &str,
    source_output: &str,
    tenant_id: &str,
    origin: Option<PipelineOrigin>,
    app_state: &AppState,
) -> Result<Vec<String>, String> {
    let __pool_1 = app_state.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let store = PipelineStore::new(&__pool_1);
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

        match execute_target_agent(
            &pipeline,
            source_output,
            tenant_id,
            origin.as_ref(),
            app_state,
        )
        .await
        {
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
    origin: Option<&PipelineOrigin>,
    app_state: &AppState,
) -> Result<(), String> {
    use crate::agents::context_provider::AgentRuntimeContext;
    use crate::agents::tenant_agent;
    use crate::agents::Agent;
    use crate::observability::{
        run_cost_aggregation_request, spawn_run_cost_aggregation, RunObservability,
    };

    let pool = app_state.get::<crate::TenantDb>().expect("not provided").pool().clone();
    let scoped = tenant_scoped_ctx(app_state, tenant_id);

    let mut resolved_agent = tenant_agent::resolve_agent_from_ctx(&pool,
        &app_state.get::<ares_agents::AgentRegistry>().expect("AgentRegistry not provided"),
        &scoped,
        &pipeline.target_agent,
        &app_state.get::<crate::FleetSecrets>().expect("not provided"),
    )
    .await
    .map_err(|e| format!("Agent resolution failed: {}", e))?;

    let start = std::time::Instant::now();
    let run_id = uuid::Uuid::new_v4().to_string();
    let origin_ref = origin;

    // Skill-based execution
    if let Some(config) = &resolved_agent.config {
        if let Some(skill_id) = config.get("skill_id").and_then(|v| v.as_str()) {
            app_state.get::<crate::active_runs::ActiveRuns>().expect("not provided").start(pipeline_active_run(
                &run_id,
                tenant_id,
                &pipeline.target_agent,
                &pipeline.id,
                origin_ref,
                Some(format!("skill:{}", skill_id)),
            ));

            let skill_result = app_state.get::<crate::skill_engine::SkillEngine>().expect("not provided")
                .execute_skill(
                    skill_id,
                    tenant_id,
                    serde_json::json!({"message": source_output}),
                    &run_id,
                )
                .await;

            let duration_ms = start.elapsed().as_millis() as u64;
            let skill_status = if skill_result.is_ok() {
                "completed"
            } else {
                "error"
            };
            app_state.get::<crate::active_runs::ActiveRuns>().expect("not provided").finish(&run_id, skill_status);

            let (input_tokens, output_tokens) = skill_result
                .as_ref()
                .map(crate::skill_engine::skill_result_token_counts)
                .unwrap_or((0, 0));
            let effects = pipeline_target_run_effects(
                pipeline,
                tenant_id,
                &run_id,
                origin_ref,
                Some(resolved_agent.source.as_str()),
                resolved_agent.config_version.clone(),
                false,
                input_tokens,
                output_tokens,
                "skill",
                "skill",
            );
            let metadata = effects.metadata;
            let usage = effects.usage;
            let status = if skill_result.is_ok() {
                "completed"
            } else {
                "failed"
            };
            let err_msg = skill_result.as_ref().err().cloned();

            let pool_clone = pool.clone();
            let tid = tenant_id.to_string();
            let aname = pipeline.target_agent.clone();
            let run_id_for_insert = run_id.clone();
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
                    err_msg.as_deref(),
                    "skill",
                    "skill",
                    false,
                    Some(&metadata),
                )
                .await;
            });

            spawn_run_cost_aggregation(
                pool.clone(),
                run_cost_aggregation_request(
                    &run_id,
                    tenant_id,
                    &pipeline.target_agent,
                    duration_ms as i64,
                ),
            );

            let usage_pool = pool.clone();
            tokio::spawn(async move {
                let _ = sqlx::query(
                    "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
                )
                .bind(uuid::Uuid::new_v4().to_string())
                .bind(usage.tenant_id)
                .bind(usage.source)
                .bind(usage.request_count)
                .bind(usage.token_count)
                .bind(usage.input_tokens)
                .bind(usage.output_tokens)
                .bind(usage.model_name)
                .bind(usage.agent_name)
                .bind(usage.provider_name)
                .bind(chrono::Utc::now().timestamp())
                .execute(&usage_pool)
                .await;
            });

            return skill_result.map(|_| ());
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
    resolved_agent.agent.set_runtime_tools_from_ctx(
        app_state.get::<crate::RuntimeToolRegistry>().expect("not provided").clone(),
        &scoped,
    );

    let mut runtime_context = AgentRuntimeContext::new(
        tenant_id.to_string(),
        pipeline.target_agent.clone(),
        "pipeline",
    );
    runtime_context.session_id = Some(run_id.clone());

    let eruka_context = app_state.get::<crate::context_services::ContextProviderService>().expect("not provided").0
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

    app_state.get::<crate::active_runs::ActiveRuns>().expect("not provided").start(pipeline_active_run(
        &run_id,
        tenant_id,
        &pipeline.target_agent,
        &pipeline.id,
        origin_ref,
        None,
    ));

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
            app_state.get::<crate::active_runs::ActiveRuns>().expect("not provided")
                .update_model(&run_id, Some(&model_name));
            app_state.get::<crate::active_runs::ActiveRuns>().expect("not provided").finish(&run_id, "completed");
        }
        Err(e) => {
            status = "failed";
            error_msg = Some(e.to_string());
            input_tokens = 0;
            output_tokens = 0;
            model_name = "unknown".to_string();
            provider_name = "unknown".to_string();
            app_state.get::<crate::active_runs::ActiveRuns>().expect("not provided").finish(&run_id, "error");
        }
    }

    let effects = pipeline_target_run_effects(
        pipeline,
        tenant_id,
        &run_id,
        origin_ref,
        Some(resolved_agent.source.as_str()),
        resolved_agent.config_version.clone(),
        eruka_context_hit,
        input_tokens,
        output_tokens,
        &model_name,
        &provider_name,
    );
    let metadata = effects.metadata;
    let usage = effects.usage;

    let pool_clone = pool.clone();
    let tid = tenant_id.to_string();
    let aname = pipeline.target_agent.clone();
    let err_clone = error_msg.clone();
    let run_id_for_insert = run_id.clone();
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

    let usage_pool = pool.clone();
    tokio::spawn(async move {
        let _ = sqlx::query(
            "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(usage.tenant_id)
        .bind(usage.source)
        .bind(usage.request_count)
        .bind(usage.token_count)
        .bind(usage.input_tokens)
        .bind(usage.output_tokens)
        .bind(usage.model_name)
        .bind(usage.agent_name)
        .bind(usage.provider_name)
        .bind(chrono::Utc::now().timestamp())
        .execute(&usage_pool)
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
    fn tenant_scoped_ctx_sets_isolate_label() {
        use std::any::TypeId;
        let root = Context::new_root();
        let scoped = tenant_scoped_ctx(&root, "acme");
        assert_eq!(
            scoped.isolate_label(TypeId::of::<ares_agents::AgentResolverService>()).as_deref(),
            Some("tenant:acme"),
        );
        assert!(root.isolate_label(TypeId::of::<ares_agents::AgentResolverService>()).is_none());
    }

    #[test]
    fn test_evaluate_condition() {
        assert!(evaluate_condition(
            "output.contains(\"hello\")",
            "hello world"
        ));
        assert!(!evaluate_condition(
            "output.contains(\"xyz\")",
            "hello world"
        ));
        assert!(evaluate_condition(
            "output.starts_with(\"hello\")",
            "hello world"
        ));
        assert!(evaluate_condition(
            "output.ends_with(\"world\")",
            "hello world"
        ));
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

    #[test]
    fn pipeline_target_run_effects_preserve_scheduled_origin() {
        let pipeline = AgentPipeline {
            id: "pipeline-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            source_agent: "source".to_string(),
            target_agent: "target".to_string(),
            condition: None,
            enabled: true,
            created_at: 1,
            updated_at: 1,
        };
        let origin = PipelineOrigin::scheduled("schedule-1".to_string(), true);

        let effects = pipeline_target_run_effects(
            &pipeline,
            "tenant-1",
            "run-1",
            Some(&origin),
            Some("tenant-db"),
            Some("v1".to_string()),
            false,
            1,
            2,
            "model",
            "provider",
        );

        assert_eq!(effects.metadata.request_source.as_deref(), Some("pipeline"));
        assert_eq!(effects.metadata.pipeline_id.as_deref(), Some("pipeline-1"));
        assert_eq!(effects.metadata.schedule_id.as_deref(), Some("schedule-1"));
        assert_eq!(effects.metadata.trigger_id, None);
    }

    #[test]
    fn pipeline_active_run_preserves_scheduled_origin() {
        let origin = PipelineOrigin::scheduled("schedule-1".to_string(), true);
        let run = pipeline_active_run(
            "run-1",
            "tenant-1",
            "target",
            "pipeline-1",
            Some(&origin),
            None,
        );

        assert!(run.is_catchup);
        assert_eq!(run.request_source.as_deref(), Some("pipeline"));
        assert_eq!(run.pipeline_id.as_deref(), Some("pipeline-1"));
        assert_eq!(run.schedule_id.as_deref(), Some("schedule-1"));
        assert_eq!(run.trigger_id, None);
    }

    #[test]
    fn pipeline_active_run_preserves_trigger_origin() {
        let origin = PipelineOrigin::trigger("trigger-1".to_string());
        let run = pipeline_active_run(
            "run-1",
            "tenant-1",
            "target",
            "pipeline-1",
            Some(&origin),
            Some("skill:child".to_string()),
        );

        assert!(!run.is_catchup);
        assert_eq!(run.pipeline_id.as_deref(), Some("pipeline-1"));
        assert_eq!(run.schedule_id, None);
        assert_eq!(run.trigger_id.as_deref(), Some("trigger-1"));
        assert_eq!(run.tool_name.as_deref(), Some("skill:child"));
    }

    #[test]
    fn pipeline_target_run_effects_preserve_trigger_origin() {
        let pipeline = AgentPipeline {
            id: "pipeline-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            source_agent: "source".to_string(),
            target_agent: "target".to_string(),
            condition: None,
            enabled: true,
            created_at: 1,
            updated_at: 1,
        };
        let origin = PipelineOrigin::trigger("trigger-1".to_string());

        let effects = pipeline_target_run_effects(
            &pipeline,
            "tenant-1",
            "run-1",
            Some(&origin),
            Some("tenant-db"),
            Some("v1".to_string()),
            false,
            1,
            2,
            "model",
            "provider",
        );

        assert_eq!(effects.metadata.pipeline_id.as_deref(), Some("pipeline-1"));
        assert_eq!(effects.metadata.schedule_id, None);
        assert_eq!(effects.metadata.trigger_id.as_deref(), Some("trigger-1"));
    }
}
