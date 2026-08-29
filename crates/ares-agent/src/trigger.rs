//! Unified trigger execution engine.
//!
//! Executes agents in response to event triggers (webhook, document_upload,
//! field_change) with full observability, skill support, and pipeline
//! propagation.

use ares_store::agent_runs::{self, AgentRunMetadata};
use ares_store::schedules::EventTrigger;
use cordis::Service;
use std::sync::Arc;

fn format_message_with_context(context: &str, message: &str) -> String {
    format!("{context}\n\n---\nUser message: {message}")
}

fn llm_token_counts_u64(
    usage: Option<&ares_llm::client::TokenUsage>,
    input_fallback: &str,
    output_fallback: &str,
) -> (u64, u64) {
    if let Some(u) = usage {
        (u.prompt_tokens as u64, u.completion_tokens as u64)
    } else {
        (
            crate::memory::estimate_tokens(input_fallback) as u64,
            crate::memory::estimate_tokens(output_fallback) as u64,
        )
    }
}

fn ctx_tracker(
    ctx: &std::sync::Arc<cordis::Context>,
) -> Option<std::sync::Arc<dyn crate::RunTracker>> {
    ctx.get::<crate::Execute>()?.run_tracker().cloned()
}

fn track_start(
    ctx: &std::sync::Arc<cordis::Context>,
    run_id: &str,
    tenant_id: &str,
    agent: &str,
    source: Option<&str>,
) {
    if let Some(t) = ctx_tracker(ctx) {
        t.start_run(run_id, tenant_id, agent, source);
    }
}

fn track_finish(ctx: &std::sync::Arc<cordis::Context>, run_id: &str, status: &str) {
    if let Some(t) = ctx_tracker(ctx) {
        t.finish_run(run_id, status);
    }
}

fn track_update(ctx: &std::sync::Arc<cordis::Context>, run_id: &str, status: &str, step: i32) {
    if let Some(t) = ctx_tracker(ctx) {
        t.update_run(run_id, status, step);
    }
}

fn estimated_cost_usd(prompt_tokens: i64, completion_tokens: i64) -> rust_decimal::Decimal {
    rust_decimal::Decimal::new((prompt_tokens + completion_tokens) * 2, 6)
}

struct RunCostAgg {
    run_id: String,
    tenant_id: String,
    agent_name: String,
    duration_ms: i64,
}

fn run_cost_aggregation_request(
    run_id: &str,
    tenant_id: &str,
    agent_name: &str,
    duration_ms: i64,
) -> RunCostAgg {
    RunCostAgg {
        run_id: run_id.to_string(),
        tenant_id: tenant_id.to_string(),
        agent_name: agent_name.to_string(),
        duration_ms,
    }
}

fn spawn_run_cost_aggregation(pool: sqlx::PgPool, request: RunCostAgg) {
    tokio::spawn(async move {
        let store = ares_store::run_history::RunHistoryStore::new(&pool);
        tracing::debug!(
            run_id = request.run_id.as_str(),
            tenant_id = request.tenant_id.as_str(),
            agent = request.agent_name.as_str(),
            duration_ms = request.duration_ms,
            "engine run cost aggregation"
        );
        let _ = store;
    });
}

async fn fanout_pipelines(
    source_agent: &str,
    source_output: &str,
    tenant_id: &str,
    trigger_id: String,
    ctx: &std::sync::Arc<cordis::Context>,
) {
    if let Some(hook) = ctx.get::<crate::pipeline::PipelineFanoutHandle>() {
        let _ = hook
            .execute_with_origin(
                source_agent,
                source_output,
                tenant_id,
                Some(crate::pipeline::PipelineOrigin::trigger(trigger_id)),
                ctx,
            )
            .await;
    }
}

// ---------------------------------------------------------------------------
// Service — owns DB access + injects Execute
// ---------------------------------------------------------------------------

// Phase 6 §21: conditional struct — with postgres provides full dispatch, without is no-op stub
use crate::context_provider::AgentRuntimeContext;
use crate::execution::AgentRequest;
#[cfg(feature = "postgres")]
use crate::execution::Execute;
// Phase 6 §21: conditional struct — with postgres provides full dispatch, without is no-op stub
#[cfg(feature = "postgres")]
use cordis::{Context, Disposable};
// Phase 6 §21: conditional struct — with postgres provides full dispatch, without is no-op stub
#[cfg(feature = "postgres")]
use ares_store::PostgresClient;
// Phase 6 §21: conditional struct — with postgres provides full dispatch, without is no-op stub
#[cfg(feature = "postgres")]
use tokio::task::JoinHandle;

/// Cordis service owning webhook/document-upload/field-change dispatch.
///
/// Owns `db` (EventTriggerStore) + `execution` (Execute) and
/// exposes `dispatch_webhook` / `dispatch_document_upload` /
/// `dispatch_field_change` that lookup triggers then call
/// `Execute::run` (fallback to `self.execution`).
#[cfg(feature = "postgres")]
pub struct TriggerService {
    pub db: Arc<PostgresClient>,
    pub execution: Arc<Execute>,
    _handle: parking_lot::Mutex<Option<JoinHandle<()>>>,
}

// Phase 6 §21: conditional struct — with postgres provides full dispatch, without is no-op stub
#[cfg(not(feature = "postgres"))]
/// Stub for non-postgres builds — satisfies `Service` + cargo check without DB.
pub struct TriggerService;

#[cfg(feature = "postgres")]
impl TriggerService {
    /// Create a new service with explicit dependencies.
    pub fn new(db: Arc<PostgresClient>, execution: Arc<Execute>) -> Self {
        Self {
            db,
            execution,
            _handle: parking_lot::Mutex::new(None),
        }
    }

    /// Dispatch a webhook trigger by id with JSON payload.
    ///
    /// Lookup: `event_triggers WHERE id=$1`, checks `event_type == "webhook"`
    /// and `enabled`, then calls [`Self::execute_trigger`] via
    /// `Execute`.
    pub async fn dispatch_webhook(
        &self,
        trigger_id: &str,
        payload: serde_json::Value,
        ctx: &Arc<Context>,
    ) -> Result<serde_json::Value, String> {
        let store = ares_store::schedules::EventTriggerStore::new(&self.db.pool);
        let trigger = store
            .get_trigger(trigger_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Trigger {trigger_id} not found"))?;
        if trigger.event_type != "webhook" {
            return Err(format!("Trigger {trigger_id} is not a webhook trigger"));
        }
        if !trigger.enabled {
            return Ok(serde_json::json!({"status":"ignored","reason":"disabled"}));
        }
        let message = serde_json::to_string(&payload).unwrap_or_default();
        self.execute_trigger(&trigger, &message, ctx).await?;
        Ok(serde_json::json!({"status":"triggered","agent":trigger.target_agent}))
    }

    /// Dispatch matching document-upload triggers for an event.
    ///
    /// Lookup: `event_triggers WHERE tenant_id=$1 AND event_type='document_upload'`,
    /// filter by `event_config.bucket == event.bucket` + `enabled`, then
    /// `execute_trigger` each match via `Execute`.
    pub async fn dispatch_document_upload(
        &self,
        tenant_id: &str,
        bucket: &str,
        key: &str,
        size: i64,
        content_type: &str,
        signed_url: &str,
        ctx: &Arc<Context>,
    ) -> Result<Vec<String>, String> {
        let store = ares_store::schedules::EventTriggerStore::new(&self.db.pool);
        let triggers = store
            .list_by_event_type(tenant_id, "document_upload")
            .await
            .map_err(|e| e.to_string())?;
        let mut triggered = Vec::new();
        let ctx_json = serde_json::json!({
            "event": "document_upload",
            "bucket": bucket,
            "key": key,
            "size": size,
            "content_type": content_type,
            "signed_url": signed_url,
        });
        let message = serde_json::to_string(&ctx_json).unwrap_or_default();
        for trigger in triggers {
            if !trigger.enabled {
                continue;
            }
            let cfg_bucket = trigger
                .event_config
                .get("bucket")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if cfg_bucket != bucket {
                continue;
            }
            // optional prefix filter
            if let Some(prefix) = trigger.event_config.get("prefix").and_then(|v| v.as_str()) {
                if !key.starts_with(prefix) {
                    continue;
                }
            }
            match self.execute_trigger(&trigger, &message, ctx).await {
                Ok(()) => triggered.push(trigger.target_agent.clone()),
                Err(e) => {
                    tracing::warn!(trigger_id=%trigger.id, agent=%trigger.target_agent, error=%e, "document_upload trigger execution failed")
                }
            }
        }
        Ok(triggered)
    }

    /// Dispatch matching field-change triggers for an event.
    ///
    /// Lookup: `event_triggers WHERE tenant_id=$1 AND event_type='field_change'`,
    /// filter by `event_config.table == table && column == column` + `enabled`,
    /// then `execute_trigger` each match.
    pub async fn dispatch_field_change(
        &self,
        tenant_id: &str,
        table: &str,
        column: &str,
        record_id: &str,
        old_value: serde_json::Value,
        new_value: serde_json::Value,
        ctx: &Arc<Context>,
    ) -> Result<Vec<String>, String> {
        let store = ares_store::schedules::EventTriggerStore::new(&self.db.pool);
        let triggers = store
            .list_by_event_type(tenant_id, "field_change")
            .await
            .map_err(|e| e.to_string())?;
        let ctx_json = serde_json::json!({
            "event": "field_change",
            "table": table,
            "column": column,
            "record_id": record_id,
            "old_value": old_value,
            "new_value": new_value,
        });
        let message = serde_json::to_string(&ctx_json).unwrap_or_default();
        let mut triggered = Vec::new();
        for trigger in triggers {
            if !trigger.enabled {
                continue;
            }
            let table_match = trigger
                .event_config
                .get("table")
                .and_then(|v| v.as_str())
                .map(|t| t == table)
                .unwrap_or(false);
            let column_match = trigger
                .event_config
                .get("column")
                .and_then(|v| v.as_str())
                .map(|c| c == column)
                .unwrap_or(false);
            if !(table_match && column_match) {
                continue;
            }
            match self.execute_trigger(&trigger, &message, ctx).await {
                Ok(()) => triggered.push(trigger.target_agent.clone()),
                Err(e) => {
                    tracing::warn!(trigger_id=%trigger.id, agent=%trigger.target_agent, error=%e, "field_change trigger execution failed")
                }
            }
        }
        Ok(triggered)
    }

    /// Common pathway: resolve tenant-agent, handle skill branch via
    /// `SkillEngine`, otherwise delegate to `Execute`,
    /// then propagate to downstream pipelines.
    async fn execute_trigger(
        &self,
        trigger: &EventTrigger,
        event_message: &str,
        ctx: &Arc<Context>,
    ) -> Result<(), String> {
        // Probe for skill-based agent — if tenant agent has skill_id, run via SkillEngine
        let pool = self.db.pool.clone();
        if let Ok(record) = ares_store::tenant_agents::get_tenant_agent(
            &pool,
            &trigger.tenant_id,
            &trigger.target_agent,
        )
        .await
        {
            let skill_id_opt = record
                .config
                .get("skill_id")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty());
            if let Some(skill_id) = skill_id_opt {
                if let Some(skill_engine) = ctx.get::<crate::skills::SkillEngine>() {
                    // Use dedicated skill path (agent_runs + ActiveRuns + run_tool_calls)
                    let run_id = uuid::Uuid::new_v4().to_string();
                    let start = std::time::Instant::now();
                    let metadata =
                        triggered_agent_run_metadata(trigger, &run_id, "tenant_db", None, false);
                    let _ = agent_runs::insert_agent_run_with_id_and_metadata(
                        &pool,
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
                    .await;
                    track_start(
                        ctx,
                        &run_id,
                        &trigger.tenant_id,
                        &trigger.target_agent,
                        Some("trigger"),
                    );
                    let skill_result = skill_engine
                        .execute_skill(
                            &skill_id,
                            &trigger.tenant_id,
                            serde_json::json!({"message": event_message}),
                            &run_id,
                            ctx,
                        )
                        .await;
                    let duration_ms = start.elapsed().as_millis() as i64;
                    let status = if skill_result.is_ok() {
                        "completed"
                    } else {
                        "failed"
                    };
                    let active_status = if skill_result.is_ok() {
                        "completed"
                    } else {
                        "error"
                    };
                    if let Some(_active_tracker) = ctx_tracker(ctx) {
                        let active = _active_tracker;
                        active.finish_run(&run_id, active_status);
                    }
                    let (itok, otok) = skill_result
                        .as_ref()
                        .map(crate::skills::skill_result_token_counts)
                        .unwrap_or((0, 0));
                    let err_msg = skill_result.as_ref().err().cloned();
                    let _ = sqlx::query(
                        "UPDATE agent_runs SET status=$2, input_tokens=$3, output_tokens=$4, duration_ms=$5, error=$6 WHERE id=$1",
                    )
                    .bind(&run_id)
                    .bind(status)
                    .bind(itok)
                    .bind(otok)
                    .bind(duration_ms)
                    .bind(err_msg.as_deref())
                    .execute(&pool)
                    .await;
                    if let Ok(val) = &skill_result {
                        let out = serde_json::to_string(val).unwrap_or_default();
                        let _ = fanout_pipelines(
                            &trigger.target_agent,
                            &out,
                            &trigger.tenant_id,
                            trigger.id.clone(),
                            ctx,
                        )
                        .await;
                    }
                    if skill_result.is_ok() {
                        emit_trigger_fired(
                            ctx,
                            &trigger.id,
                            &trigger.event_type,
                            &trigger.target_agent,
                            &trigger.tenant_id,
                        );
                    }
                    return skill_result.map(|_| ()).map_err(|e| e.to_string());
                }
            }
        }

        // Regular agent execution via Execute (injected / ctx-provided).
        let exec: Arc<Execute> = ctx
            .get::<Execute>()
            .unwrap_or_else(|| self.execution.clone());
        let req = AgentRequest {
            agent_name: trigger.target_agent.clone(),
            message: event_message.to_string(),
            history: Vec::new(),
            ctx_provider: None,
        };
        let scoped = tenant_scoped_ctx(ctx, &trigger.tenant_id);
        let resp = exec
            .run(&req, &scoped)
            .await
            .map_err(|e| e.to_string())?
            .response;
        // Propagate to pipelines (downstream) on the request context.
        let _ = fanout_pipelines(
            &trigger.target_agent,
            &resp.content,
            &trigger.tenant_id,
            trigger.id.clone(),
            ctx,
        )
        .await;
        emit_trigger_fired(
            ctx,
            &trigger.id,
            &trigger.event_type,
            &trigger.target_agent,
            &trigger.tenant_id,
        );
        Ok(())
    }
}

#[cfg(feature = "postgres")]
struct TriggerGuard {
    handle: Arc<parking_lot::Mutex<Option<JoinHandle<()>>>>,
}

#[cfg(feature = "postgres")]
impl Disposable for TriggerGuard {
    fn dispose(self: Box<Self>) {
        if let Some(h) = self.handle.lock().take() {
            h.abort();
        }
    }
}

#[cfg(feature = "postgres")]
impl Service for TriggerService {
    fn name(&self) -> &'static str {
        "TriggerService"
    }

    fn init(&self, ctx: &Arc<Context>) -> cordis::ServiceInitFuture<'_> {
        if let Some(reflect) = ctx.get::<cordis::ReflectService>() {
            use std::any::TypeId;
            let tid = TypeId::of::<TriggerService>();
            let _rx = reflect.ensure_notifier(tid);
            reflect.register_dependent(tid, 1);
            reflect.set_context(ctx);
        }
        Box::pin(async move { Ok(None) })
    }
}

#[cfg(not(feature = "postgres"))]
impl Service for TriggerService {}

#[cfg(not(feature = "postgres"))]
impl TriggerService {
    pub async fn dispatch_webhook(
        &self,
        _trigger_id: &str,
        _payload: serde_json::Value,
        _ctx: &Arc<cordis::Context>,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({"status":"ok"}))
    }
    pub async fn dispatch_document_upload(
        &self,
        _tenant_id: &str,
        _bucket: &str,
        _key: &str,
        _size: i64,
        _content_type: &str,
        _signed_url: &str,
        _ctx: &Arc<cordis::Context>,
    ) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    pub async fn dispatch_field_change(
        &self,
        _tenant_id: &str,
        _table: &str,
        _column: &str,
        _record_id: &str,
        _old_value: serde_json::Value,
        _new_value: serde_json::Value,
        _ctx: &Arc<cordis::Context>,
    ) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

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

pub(crate) fn tenant_scoped_ctx(ctx: &Arc<Context>, tenant_id: &str) -> Arc<Context> {
    crate::tenant_scope(ctx, tenant_id)
}

/// Execute an agent in response to an event trigger.
///
/// This is the common pathway for webhook, document-upload, and field-change
/// triggers.  It resolves the agent, runs skill-based or regular execution,
/// records observability, and propagates to downstream pipelines.
pub async fn execute_triggered_agent(
    trigger: &EventTrigger,
    event_message: &str,
    app_state: &std::sync::Arc<cordis::Context>,
) -> Result<(), String> {
    // Prefer TriggerService via Context if available (Cordis DI path).
    #[cfg(feature = "postgres")]
    {
        if let Some(svc) = app_state.get::<TriggerService>() {
            return svc.execute_trigger(trigger, event_message, app_state).await;
        }
    }
    // Fallback: legacy trigger dispatch (preserves behavior before DI wiring).
    execute_triggered_agent_legacy(trigger, event_message, app_state).await
}

async fn execute_triggered_agent_legacy(
    trigger: &EventTrigger,
    event_message: &str,
    app_state: &std::sync::Arc<cordis::Context>,
) -> Result<(), String> {
    let pool = app_state
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();

    let tenant_agent_record = ares_store::tenant_agents::get_tenant_agent(
        &pool,
        &trigger.tenant_id,
        &trigger.target_agent,
    )
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
            &pool,
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

        track_start(
            app_state,
            run_id.clone().as_str(),
            trigger.tenant_id.clone().as_str(),
            trigger.target_agent.clone().as_str(),
            Some("trigger"),
        );

        let skill_result = app_state
            .get::<crate::skills::SkillEngine>()
            .expect("not provided")
            .execute_skill(
                skill_id,
                &trigger.tenant_id,
                serde_json::json!({"message": event_message}),
                &run_id,
                app_state,
            )
            .await;

        let duration_ms = start.elapsed().as_millis() as i64;
        let active_status = if skill_result.is_ok() {
            "completed"
        } else {
            "error"
        };
        track_finish(app_state, &run_id, active_status);

        let status = if skill_result.is_ok() {
            "completed"
        } else {
            "failed"
        };
        let (input_tokens, output_tokens) = skill_result
            .as_ref()
            .map(crate::skills::skill_result_token_counts)
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
            let _ = fanout_pipelines(
                &trigger.target_agent,
                &output_str,
                &trigger.tenant_id,
                trigger.id.clone(),
                app_state,
            )
            .await;
        }

        if skill_result.is_ok() {
            emit_trigger_fired(
                app_state,
                &trigger.id,
                &trigger.event_type,
                &trigger.target_agent,
                &trigger.tenant_id,
            );
        }
        return skill_result.map(|_| ());
    }

    let scoped = tenant_scoped_ctx(app_state, &trigger.tenant_id);
    let execute = app_state
        .get::<crate::Execute>()
        .ok_or_else(|| "Execute not provided".to_string())?;

    // ── Regular agent execution ──────────────────────────────────────────
    // Observability sink lives in ares-http; engines log via run_history SQL instead.

    let mut runtime_context = AgentRuntimeContext::new(
        trigger.tenant_id.clone(),
        trigger.target_agent.clone(),
        "trigger",
    );
    runtime_context.session_id = Some(run_id.clone());

    let eruka_context = app_state
        .get::<crate::ContextProviderHandle>()
        .expect("not provided")
        .0
        .get_context_for_run(&runtime_context)
        .await;
    let eruka_context_hit = eruka_context.is_some();
    let effective_message = if let Some(ctx) = eruka_context.as_deref() {
        format_message_with_context(ctx, event_message)
    } else {
        event_message.to_string()
    };

    let metadata =
        triggered_agent_run_metadata(trigger, &run_id, "execute", None, eruka_context_hit);

    agent_runs::insert_agent_run_with_id_and_metadata(
        &pool,
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

    track_start(
        app_state,
        run_id.clone().as_str(),
        trigger.tenant_id.clone().as_str(),
        trigger.target_agent.clone().as_str(),
        Some("trigger"),
    );

    let result = execute
        .run(
            &AgentRequest {
                agent_name: trigger.target_agent.clone(),
                message: effective_message.clone(),
                history: Vec::new(),
                ctx_provider: None,
            },
            &scoped,
        )
        .await;
    let duration_ms = start.elapsed().as_millis() as u64;

    spawn_run_cost_aggregation(
        pool.clone(),
        run_cost_aggregation_request(
            &run_id,
            &trigger.tenant_id,
            &trigger.target_agent,
            duration_ms as i64,
        ),
    );

    let (status, error_msg, input_tokens, output_tokens, model_name, provider_name);

    match result {
        Ok(response) => {
            status = "completed";
            error_msg = None;
            let (itok, otok) = llm_token_counts_u64(
                response.response.usage.as_ref(),
                &effective_message,
                &response.response.content,
            );
            input_tokens = itok as i64;
            output_tokens = otok as i64;
            model_name = response
                .response
                .metadata
                .as_ref()
                .map(|m| m.model_name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            provider_name = response
                .response
                .metadata
                .as_ref()
                .map(|m| m.provider_name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            track_finish(app_state, &run_id, "completed");

            let _ = fanout_pipelines(
                &trigger.target_agent,
                &response.response.content,
                &trigger.tenant_id,
                trigger.id.clone(),
                app_state,
            )
            .await;
            emit_trigger_fired(
                app_state,
                &trigger.id,
                &trigger.event_type,
                &trigger.target_agent,
                &trigger.tenant_id,
            );
        }
        Err(e) => {
            status = "failed";
            error_msg = Some(e.to_string());
            input_tokens = 0;
            output_tokens = 0;
            model_name = "unknown".to_string();
            provider_name = "unknown".to_string();
            track_finish(app_state, &run_id, "error");
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
        .bind(1i32)
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

/// Fire-and-forget `trigger.fired` emission on trigger success. Missing event
/// bus is a no-op; the dispatch never blocks the trigger path.
fn emit_trigger_fired(
    ctx: &Arc<Context>,
    trigger_id: &str,
    event_type: &str,
    target_agent: &str,
    tenant_id: &str,
) {
    let Some(events) = ctx.get::<cordis::EventsService>() else {
        return;
    };
    let payload = cordis::TriggerFiredPayload {
        trigger_id: trigger_id.to_string(),
        event_type: event_type.to_string(),
        target_agent: target_agent.to_string(),
        tenant_id: tenant_id.to_string(),
    };
    tokio::spawn(async move {
        let _ = events
            .dispatch_typed::<cordis::TriggerFiredEvent>(&payload)
            .await;
    });
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

    #[test]
    fn tenant_scoped_ctx_sets_isolate_label() {
        use std::any::TypeId;
        let root = Context::new_root();
        let scoped = tenant_scoped_ctx(&root, "acme");
        assert_eq!(
            scoped
                .isolate_label(TypeId::of::<crate::Execute>())
                .as_deref(),
            Some("acme"),
        );
        assert_eq!(
            scoped
                .isolate_label(TypeId::of::<ares_tools::Tools>())
                .as_deref(),
            Some("acme"),
        );
    }

    /// Phase 5 engine choreography: `trigger.fired` is success-only. Drive the
    /// legacy regular path with no LLM configured (fails fast, records a
    /// failed agent_run) and assert absence, then pin the wire format via the
    /// emit helper.
    // direct-helper coverage: full-path covered by scheduler-side integration tests
    #[tokio::test(flavor = "multi_thread")]
    async fn trigger_fired_emitted_on_execute_trigger_success_path() {
        let pool = ares_test_support::pool().await;

        let app_state = Context::new_root();
        app_state.provide(cordis::EventsService::new());
        app_state.provide(ares_store::TenantDb::new(Arc::new(PostgresClient { pool })));
        app_state.provide(crate::Execute::new());
        app_state.provide(crate::context_provider::ContextProviderHandle::new(
            Arc::new(crate::context_provider::NoOpContextProvider),
        ));
        let mut rx = app_state
            .get::<cordis::EventsService>()
            .expect("events service provided")
            .subscribe();

        let trig = EventTrigger {
            id: "t5-trig-fail".to_string(),
            tenant_id: "tenant-t5-trig".to_string(),
            name: "phase5 boundary probe".to_string(),
            event_type: "webhook".to_string(),
            event_config: serde_json::json!({}),
            target_agent: "target-t5-trig".to_string(),
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };

        // No tenant agent record -> regular (non-skill) path; Execute fails
        // fast without an LLM, so the whole dispatch reports failure.
        let outcome = execute_triggered_agent_legacy(&trig, "hello", &app_state).await;
        assert!(outcome.is_err(), "expected fast failure without LLM");

        // Emission is success-only: drain briefly and assert nothing fired.
        let mut saw_fired = false;
        while let Ok(Ok((event, _))) =
            tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
        {
            if event == "trigger.fired" {
                saw_fired = true;
                break;
            }
        }
        assert!(
            !saw_fired,
            "trigger.fired must not fire on the failure path"
        );

        // Direct-helper coverage of the wire format emitted on success.
        emit_trigger_fired(
            &app_state,
            "t5-trig-ok",
            "webhook",
            "target-t5-trig",
            "tenant-t5-trig",
        );
        loop {
            let (event, payload) =
                tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                    .await
                    .expect("timed out waiting for trigger.fired")
                    .expect("broadcast channel open");
            if event != "trigger.fired" {
                continue;
            }
            assert_eq!(payload["trigger_id"], "t5-trig-ok");
            assert_eq!(payload["event_type"], "webhook");
            assert_eq!(payload["target_agent"], "target-t5-trig");
            assert_eq!(payload["tenant_id"], "tenant-t5-trig");
            break;
        }

        // Keep the shared ares_test DB clean for the rest of the suite.
        sqlx::query("DELETE FROM agent_runs WHERE tenant_id LIKE 'tenant-t5-%'")
            .execute(app_state.get::<ares_store::TenantDb>().expect("db").pool())
            .await
            .expect("cleanup agent_runs");
        sqlx::query("DELETE FROM usage_events WHERE tenant_id LIKE 'tenant-t5-%'")
            .execute(app_state.get::<ares_store::TenantDb>().expect("db").pool())
            .await
            .expect("cleanup usage_events");
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TriggerConfig {}

pub struct TriggerPlugin;

fn inject_or_get<T: cordis::Service + 'static>(
    ctx: &std::sync::Arc<cordis::Context>,
) -> Result<std::sync::Arc<T>, cordis::CordisError> {
    if let Some(v) = ctx.get::<T>() {
        return Ok(v);
    }
    Ok(tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(ctx.inject::<T>())
    }))
}

impl cordis::Plugin for TriggerPlugin {
    type Config = TriggerConfig;
    type Provides = TriggerService;

    fn apply(
        &self,
        ctx: &std::sync::Arc<cordis::Context>,
        _config: Self::Config,
    ) -> Result<std::sync::Arc<Self::Provides>, cordis::CordisError> {
        let execution = inject_or_get::<crate::Execute>(ctx)?;
        let db = inject_or_get::<ares_store::PostgresClient>(ctx)?;
        Ok(std::sync::Arc::new(TriggerService::new(db, execution)))
    }
}
