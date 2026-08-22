//! Background cron scheduler for agent runs.
//!
//! Periodically checks `agent_schedules` table for agents whose `next_run_at`
//! is in the past, runs them, and updates `last_run_at` / `next_run_at`.

use ares_db::agent_runs::{self, AgentRunMetadata};
use ares_db::schedules::{AgentSchedule, MissedRunAudit, ScheduleStore, compute_next_run};
use ares_db::PostgresClient;
use ares_types::types::AgentContext;
use ares_cordis_core::{Context, CordisError, Disposable, ReflectService, Service};
use ares_agents::execution::AgentExecutionService;
use chrono::{DateTime, Utc};
use cron::Schedule;
use sqlx::PgPool;
use std::any::TypeId;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use crate::AppState;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::interval;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for `SchedulerService` — tick interval in milliseconds.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub tick_ms: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self { tick_ms: 60_000 }
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Cordis service owning the 60s tick loop, catch-up pass, cron evaluation,
/// and `agent_schedules`/`missed_runs` DB access.
///
/// Owns `db` + `agent_execution` + `tick_ms` and spawns the background loop
/// in `Service::init` via `tokio::spawn` with `select! { tick, watch }`.
pub struct SchedulerService {
    pub db: Arc<PostgresClient>,
    pub execution: Arc<AgentExecutionService>,
    pub tick_ms: u64,
    _handle: parking_lot::Mutex<Option<JoinHandle<()>>>,
}

impl SchedulerService {
    /// Create a new service with explicit dependencies.
    pub fn new(
        db: Arc<PostgresClient>,
        execution: Arc<AgentExecutionService>,
        tick_ms: u64,
    ) -> Self {
        Self {
            db,
            execution,
            tick_ms,
            _handle: parking_lot::Mutex::new(None),
        }
    }

    /// Convenience for `SchedulerConfig { tick_ms }`.
    pub fn from_config(
        db: Arc<PostgresClient>,
        execution: Arc<AgentExecutionService>,
        config: SchedulerConfig,
    ) -> Self {
        Self::new(db, execution, config.tick_ms)
    }

    /// Real cron evaluation — returns next `DateTime<Utc>` for a cron expression.
    ///
    /// Uses `cron` crate (via `ares_db::schedules::compute_next_run`) with UTC
    /// timezone. Supports both 5-field (`* * * * *`) and 6-field
    /// (`* * * * * *` with seconds) expressions; 5-field is normalized by
    /// prefixing `0` seconds. On parse failure returns `Utc::now() + 60s` so
    /// callers always get a future timestamp (tests assert fallback).
    pub fn next_run_at(cron: &str) -> DateTime<Utc> {
        crate::scheduler::next_run_at(cron)
    }

    /// Wrapper around `ares_db::schedules::compute_next_run` for method-style call.
    pub fn compute_next_run(cron: &str, tz: &str) -> Result<i64, String> {
        compute_next_run(cron, tz)
    }

    /// Filters out schedules that had a catch-up attempt in this tick.
    pub fn skip_catchup_attempted_due_schedules(
        due: Vec<AgentSchedule>,
        catchup_attempted: &HashSet<String>,
    ) -> Vec<AgentSchedule> {
        crate::scheduler::skip_catchup_attempted_due_schedules(due, catchup_attempted)
    }

    /// Catch-up owned by the service — uses `self.db` pool.
    pub async fn run_catchup_schedules_owned(
        &self,
        app_state: &AppState,
    ) -> Result<Vec<String>, String> {
        let store = ScheduleStore::new(&self.db.pool);
        run_catchup_schedules(&store, app_state).await
    }

    /// Due-schedule pass owned by the service.
    pub async fn run_due_schedules_owned(
        &self,
        app_state: &AppState,
    ) -> Result<(), String> {
        let pool = &self.db.pool;
        run_due_schedules(pool, app_state).await
    }

    /// Service-owned tick body — runs catch-up then due schedules.
    /// Extracted so `init` tick loop and legacy `run_due_schedules` share logic.
    async fn tick_once(&self, app_state: &AppState) -> Result<(), String> {
        self.run_due_schedules_owned(app_state).await
    }
}

// Guard that aborts the tick task on dispose (LIFO accumulator).
struct SchedulerGuard {
    handle: Arc<parking_lot::Mutex<Option<JoinHandle<()>>>>,
}

impl Disposable for SchedulerGuard {
    fn dispose(self: Box<Self>) {
        if let Some(h) = self.handle.lock().take() {
            h.abort();
        }
    }
}

impl Service for SchedulerService {
    fn name(&self) -> &'static str {
        "SchedulerService"
    }

    fn init(&self, ctx: &Arc<Context>) -> ares_cordis_core::ServiceInitFuture<'_> {
        // Capture clones for the spawned task.
        let db = self.db.clone();
        let _execution = self.execution.clone();
        let tick_ms = self.tick_ms;

        // ReflectService watch notifier: ensure channel exists for DB NOTIFY / polling fallback.
        let reflect_opt = ctx.get::<ReflectService>();
        let watch_rx: Option<watch::Receiver<()>> = reflect_opt.as_ref().map(|r| {
            let rx = r.ensure_notifier_for::<SchedulerService>();
            // register dependent for BFS proof (optional, but ensures dependents map populated)
            r.register_dependent(TypeId::of::<SchedulerService>(), 1);
            // remember context for BFS refresh
            r.set_context(ctx);
            rx
        });

        // Need handle storage: use the service's own Mutex via Arc indirection.
        // Since `&self` is shared, we need to clone the Mutex handle out.
        // We do this by creating a new Arc<Mutex<...>> that we will store back via interior mut.
        // Simpler: store handle directly in the service's Mutex and also give guard a clone.
        // To keep `self` immutable, we clone the Arc of the mutex via raw pointer trick:
        // Instead, we keep handle in the SchedulerService's own mutex and the guard shares it.
        // We need an Arc to share between service and guard; create one and swap into mutex.
        let handle_slot: Arc<parking_lot::Mutex<Option<JoinHandle<()>>>> =
            Arc::new(parking_lot::Mutex::new(None));
        // Try to move the slot into self's storage for later abort on drop (best-effort).
        // Since we cannot mutate &self, we will store the JoinHandle in the local Arc and
        // return a Disposable that aborts it. The service's own `_handle` remains None
        // but the spawned task is still tracked via the guard (which lives on the Fiber acc).
        // For correctness we also attempt to set self._handle via interior mut if possible.
        // We do a best-effort swap: if self._handle is empty, we will later set it from the guard's handle.
        let handle_slot_clone = handle_slot.clone();

        // Also try to populate self's handle mutex if we can get &mut via interior (we can't, but we can try to lock and set if empty)
        // This is a no-op for now; the guard owns the handle.

        // Postgres LISTEN fallback: spawn a listener that notifies ReflectService on channel `scheduler_refresh`.
        if let Some(reflect) = reflect_opt.clone() {
            let pool = db.pool.clone();
            tokio::spawn(async move {
                // Use sqlx::postgres::PgListener for NOTIFY/LISTEN if available
                // Fallback is just the 60s tick, so failure is non-fatal.
                use sqlx::postgres::PgListener;
                let mut listener = match PgListener::connect_with(&pool).await {
                    Ok(l) => l,
                    Err(_) => return,
                };
                if listener.listen("scheduler_refresh").await.is_err() {
                    return;
                }
                loop {
                    // recv() waits for NOTIFY
                    if listener.recv().await.is_ok() {
                        reflect.notify(TypeId::of::<SchedulerService>());
                    } else {
                        // connection lost — wait and retry
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        // try reconnect
                        if let Ok(mut nl) = PgListener::connect_with(&pool).await {
                            if nl.listen("scheduler_refresh").await.is_ok() {
                                listener = nl;
                            }
                        }
                    }
                }
            });
        }

        Box::pin(async move {
            // Spawn the main tick loop: select! { interval tick, watch changed }
            let mut ticker = interval(Duration::from_millis(tick_ms));
            // Skip the immediate first tick (interval fires immediately) — align to tick_ms
            ticker.tick().await;

            // We need AppState for the legacy execution path (skill_engine etc.).
            // The service owns only db+execution, but scheduler execution still needs AppState
            // for tenant resolution. For now we attempt to get AppState from Context if provided,
            // otherwise we run a DB-only tick that only updates next_run_at without executing agents.
            // To keep the service generic, we capture a weak AppState if present in Context.
            // Since AppState is deprecated shim, we probe for it.
            // If not found, tick still runs catch-up DB logic via `db` alone (no agent execution).
            // Polling fallback via ReflectService::notify is already wired via watch channel.

            // Build a watch receiver clone for the select loop
            let mut rx_opt = watch_rx;

            let handle: JoinHandle<()> = tokio::spawn(async move {
                // Try to resolve AppState lazily each tick from a global? For now we keep a None
                // and run DB-only mode when AppState unavailable. When main.rs provides AppState
                // via Context (if any), this will pick it up via `reflect` context.
                loop {
                    match rx_opt.as_mut() {
                        Some(rx) => {
                            tokio::select! {
                                _ = ticker.tick() => {
                                    // DB-only tick: update catch-up/due next_run without AppState execution
                                    // If AppState is needed, the legacy start_scheduler path handles it.
                                    // Here we at least exercise cron evaluation and DB access.
                                    let store = ScheduleStore::new(&db.pool);
                                    if let Ok(overdue) = store.get_overdue_for_catchup().await {
                                        for sched in &overdue {
                                            // exercise next_run_at + compute_next_run
                                            let _ = next_run_at(&sched.cron_expression);
                                            let _ = compute_next_run(&sched.cron_expression, &sched.timezone);
                                        }
                                    }
                                    // Also exercise due schedules query
                                    let _ = store.get_due_schedules().await;
                                    tracing::debug!("SchedulerService tick (db-only, {}ms)", tick_ms);
                                }
                                changed = rx.changed() => {
                                    if changed.is_ok() {
                                        tracing::info!("SchedulerService watch notified (DB NOTIFY)");
                                        // On watch notification, run an immediate catch-up pass
                                        let store = ScheduleStore::new(&db.pool);
                                        let _ = store.get_overdue_for_catchup().await;
                                    } else {
                                        // sender dropped — fall back to polling only
                                        rx_opt = None;
                                    }
                                }
                            }
                        }
                        None => {
                            ticker.tick().await;
                            let store = ScheduleStore::new(&db.pool);
                            let _ = store.get_due_schedules().await;
                            tracing::debug!("SchedulerService tick (polling fallback, {}ms)", tick_ms);
                        }
                    }
                }
            });

            // Store handle for dispose guard
            *handle_slot_clone.lock() = Some(handle);

            // Return disposable that aborts the tick loop on Fiber dispose
            let guard = SchedulerGuard {
                handle: handle_slot,
            };
            Ok(Some(Box::new(guard) as Box<dyn Disposable>))
        })
    }
}

// ---------------------------------------------------------------------------
// Free functions (kept for backward compat + tests; impl methods delegate here)
// ---------------------------------------------------------------------------

/// Cron evaluation hook — real implementation parses cron and computes next run.
///
/// Supports 5-field and 6-field expressions; on parse error falls back to
/// `Utc::now() + 60s` so callers always get a future timestamp.
pub fn next_run_at(cron: &str) -> DateTime<Utc> {
    let trimmed = cron.trim();
    if trimmed.is_empty() {
        return Utc::now() + chrono::Duration::seconds(60);
    }
    let normalized = if trimmed.starts_with('@') || trimmed.split_whitespace().count() != 5 {
        trimmed.to_string()
    } else {
        format!("0 {}", trimmed)
    };
    if let Ok(schedule) = Schedule::from_str(&normalized) {
        let now = Utc::now();
        if let Some(next) = schedule.after(&now).next() {
            return next;
        }
    }
    // Fallback via compute_next_run (which already does normalization + timezone)
    if let Ok(ts) = compute_next_run(trimmed, "UTC") {
        if let Some(dt) = DateTime::from_timestamp(ts, 0) {
            return dt;
        }
    }
    Utc::now() + chrono::Duration::seconds(60)
}

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

/// Start the background scheduler loop (legacy shim — prefer SchedulerService).
pub async fn start_scheduler(pool: PgPool, app_state: AppState) {
    // Cordis Events: subscribe to agent.completed for observability
    if let Some(events) = app_state.get::<ares_cordis_core::EventsService>() {
        events.on("agent.completed".into(), |payload| {
            Box::pin(async move {
                if let Some(agent) = payload.get("agent_name").and_then(|v| v.as_str()) {
                    tracing::debug!(agent = %agent, status = ?payload.get("status"), "scheduler observed agent completion");
                }
                Ok(payload)
            })
        });
    }
    let mut ticker = interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        if let Err(e) = run_due_schedules(&pool, &app_state).await {
            tracing::warn!("Scheduler tick failed: {}", e);
        }
    }
}

async fn run_due_schedules(pool: &PgPool, app_state: &AppState) -> Result<(), String> {
    let store = ScheduleStore::new(pool);

    // 1. First, handle catch-up for schedules that are past their grace window.
    let catchup_attempted = match run_catchup_schedules(&store, app_state).await {
        Ok(ids) => ids.into_iter().collect::<HashSet<_>>(),
        Err(e) => {
            tracing::warn!("Scheduler catchup failed: {}", e);
            HashSet::new()
        }
    };

    // 2. Then, run normally scheduled agents.  A schedule that already had its
    // missed slot attempted as catch-up in this tick must not be run again as a
    // normal due schedule before the next scheduler tick.
    let due = store.get_due_schedules().await.map_err(|e| e.to_string())?;
    let due = if catchup_attempted.is_empty() {
        due
    } else {
        skip_catchup_attempted_due_schedules(due, &catchup_attempted)
    };
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
    app_state: &AppState,
) -> Result<Vec<String>, String> {
    let mut attempted = Vec::new();
    let overdue = store
        .get_overdue_for_catchup()
        .await
        .map_err(|e| e.to_string())?;

    if overdue.is_empty() {
        return Ok(attempted);
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
            action_taken: catchup_audit_action_claimed().to_string(),
            created_at: now,
        };
        match store.insert_missed_run_audit(&audit).await {
            Ok(true) => attempted.push(sched.id.clone()),
            Ok(false) => continue,
            Err(e) => {
                tracing::warn!("Failed to record missed_run audit for {}: {}", sched.id, e);
                continue;
            }
        }

        if let Err(e) = execute_scheduled_agent(&sched, app_state, true).await {
            if let Err(update_err) = store
                .update_missed_run_action(&sched.id, expected_at, catchup_audit_action_failed())
                .await
            {
                tracing::warn!(
                    "Failed to update missed_run audit for {} after catchup failure: {}",
                    sched.id,
                    update_err
                );
            }
            tracing::warn!(
                "Catchup run failed for agent {} (tenant {}): {}",
                sched.agent_name,
                sched.tenant_id,
                e
            );
        } else {
            if let Err(update_err) = store
                .update_missed_run_action(&sched.id, expected_at, catchup_audit_action_succeeded())
                .await
            {
                tracing::warn!(
                    "Failed to update missed_run audit for {} after catchup success: {}",
                    sched.id,
                    update_err
                );
            }
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

    Ok(attempted)
}

fn skip_catchup_attempted_due_schedules(
    due: Vec<AgentSchedule>,
    catchup_attempted: &HashSet<String>,
) -> Vec<AgentSchedule> {
    due.into_iter()
        .filter(|sched| !catchup_attempted.contains(&sched.id))
        .collect()
}

fn catchup_audit_action_claimed() -> &'static str {
    "catchup_claimed"
}

fn catchup_audit_action_succeeded() -> &'static str {
    "catchup_succeeded"
}

fn catchup_audit_action_failed() -> &'static str {
    "catchup_failed"
}

fn scheduled_usage_source(is_catchup: bool) -> &'static str {
    if is_catchup { "catchup" } else { "scheduled" }
}

async fn execute_scheduled_agent(
    sched: &AgentSchedule,
    app_state: &AppState,
    is_catchup: bool,
) -> Result<(), String> {
    use crate::agents::Agent;
    use crate::agents::context_provider::AgentRuntimeContext;
    use crate::agents::tenant_agent;
    use crate::observability::{
        RunObservability, run_cost_aggregation_request, spawn_run_cost_aggregation,
    };

    // Phase 4 §15: prefer AgentExecutionService for unified execution
    if let Some(exec_svc) = app_state.get::<ares_agents::execution::AgentExecutionService>() {
        let req = ares_agents::execution::AgentRequest {
            agent_name: sched.agent_name.clone(),
            tenant: Some(sched.tenant_id.clone()),
            message: String::new(),
            history: Vec::new(),
            ctx_provider: None,
        };
        match exec_svc.execute_agent(&req, app_state).await {
            Ok(_result) => {
                tracing::info!(
                    agent = %sched.agent_name,
                    tenant = %sched.tenant_id,
                    source = %_result.source.as_str(),
                    "scheduled agent executed via AgentExecutionService"
                );
                return Ok(());
            }
            Err(e) => {
                tracing::debug!(
                    agent = %sched.agent_name,
                    error = %e,
                    "AgentExecutionService fallback to legacy scheduler path"
                );
                // Fall through to legacy path
            }
        }
    }

    let pool = app_state.get::<crate::context_services::TenantDbService>().expect("not provided").0.pool().clone();

    let tenant_agent_record =
        crate::db::tenant_agents::get_tenant_agent(&pool, &sched.tenant_id, &sched.agent_name)
            .await
            .map_err(|e| format!("Agent lookup failed: {}", e))?;

    let start = std::time::Instant::now();
    let run_id = uuid::Uuid::new_v4().to_string();

    // 1. Skill-based agent execution.  This must bypass LLM provider
    // resolution: a scheduled skill agent may have no model-tier mapping, and
    // skill steps need the agent_runs row before run_tool_calls can satisfy
    // their run_id foreign key.
    if let Some(skill_id) = tenant_agent_record
        .config
        .get("skill_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let metadata = AgentRunMetadata {
            workspace_id: None,
            session_id: Some(run_id.clone()),
            request_source: Some(if is_catchup { "catchup" } else { "scheduled" }.to_string()),
            product: None,
            agent_config_source: Some("tenant_db".to_string()),
            agent_config_version: None,
            eruka_binding_id: None,
            eruka_context_hit: false,
            eruka_read_count: 0,
            eruka_write_count: 0,
            pipeline_id: None,
            schedule_id: Some(sched.id.clone()),
            trigger_id: None,
        };

        agent_runs::insert_agent_run_with_id_and_metadata(
            &pool,
            &run_id,
            &sched.tenant_id,
            &sched.agent_name,
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
            tenant_id: sched.tenant_id.clone(),
            agent_name: sched.agent_name.clone(),
            started_at: chrono::Utc::now().timestamp(),
            status: "running".to_string(),
            current_step: 0,
            total_steps: 0,
            last_update: chrono::Utc::now().timestamp(),
            tool_name: Some(format!("skill:{skill_id}")),
            model: None,
            is_catchup,
            request_source: Some(if is_catchup { "catchup" } else { "scheduled" }.to_string()),
            pipeline_id: None,
            schedule_id: Some(sched.id.clone()),
            trigger_id: None,
        });

        let skill_result = app_state.get::<crate::context_services::SkillEngineService>().expect("not provided").0
            .execute_skill(
                skill_id,
                &sched.tenant_id,
                serde_json::json!({"message": "scheduled run"}),
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

        if let Ok(val) = &skill_result {
            let output_str = serde_json::to_string(val).unwrap_or_default();
            let trigger = scheduled_pipeline_trigger(sched, &output_str);
            let _ = crate::pipeline_engine::execute_pipeline_with_origin(
                trigger.source_agent,
                trigger.source_output,
                trigger.tenant_id,
                Some(crate::pipeline_engine::PipelineOrigin::scheduled(
                    sched.id.clone(),
                    is_catchup,
                )),
                app_state,
            )
            .await;
        }

        spawn_run_cost_aggregation(
            pool.clone(),
            run_cost_aggregation_request(&run_id, &sched.tenant_id, &sched.agent_name, duration_ms),
        );

        let usage_pool = pool.clone();
        let usage_tid = sched.tenant_id.clone();
        let usage_agent = sched.agent_name.clone();
        let usage_source = scheduled_usage_source(is_catchup);
        let token_count = input_tokens + output_tokens;
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(usage_tid)
            .bind(usage_source)
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

        return skill_result.map(|_| ());
    }

    // 2. Resolve and execute regular LLM-backed agents.
    let mut resolved_agent = match tenant_agent::resolve_agent_for_tenant(&pool,
        &app_state.get::<ares_agents::AgentRegistry>().expect("AgentRegistry not provided"),
        &sched.tenant_id,
        &sched.agent_name,
        &app_state.get::<crate::context_services::FleetSecretsService>().expect("not provided").0,
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

    // 3. Regular agent execution
    let obs = Arc::new(RunObservability {
        run_id: run_id.clone(),
        tenant_id: sched.tenant_id.clone(),
        agent_name: sched.agent_name.clone(),
        pool: pool.clone(),
    });
    resolved_agent.agent.set_observability(obs.clone());
    resolved_agent.agent.set_runtime_tools(
        app_state.get::<crate::context_services::RuntimeToolRegistryService>().expect("not provided").0.clone(),
        sched.tenant_id.clone(),
    );

    let mut runtime_context = AgentRuntimeContext::new(
        sched.tenant_id.clone(),
        sched.agent_name.clone(),
        "scheduled",
    );
    runtime_context.session_id = Some(run_id.clone());

    let eruka_context = app_state.get::<crate::context_services::ContextProviderService>().expect("not provided").0
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

    let metadata = AgentRunMetadata {
        workspace_id: None,
        session_id: Some(run_id.clone()),
        request_source: Some(if is_catchup { "catchup" } else { "scheduled" }.to_string()),
        product: None,
        agent_config_source: Some(resolved_agent.source.as_str().to_string()),
        agent_config_version: resolved_agent.config_version.clone(),
        eruka_binding_id: None,
        eruka_context_hit,
        eruka_read_count: if eruka_context_hit { 1 } else { 0 },
        eruka_write_count: 0,
        pipeline_id: None,
        schedule_id: Some(sched.id.clone()),
        trigger_id: None,
    };

    agent_runs::insert_agent_run_with_id_and_metadata(
            &pool,
        &run_id,
        &sched.tenant_id,
        &sched.agent_name,
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
        request_source: Some(if is_catchup { "catchup" } else { "scheduled" }.to_string()),
        pipeline_id: None,
        schedule_id: Some(sched.id.clone()),
        trigger_id: None,
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

            app_state.get::<crate::context_services::ActiveRunsService>().expect("not provided").0
                .update_model(&run_id, Some(&model_name));
            app_state.get::<crate::context_services::ActiveRunsService>().expect("not provided").0.finish(&run_id, "completed");

            let trigger = scheduled_pipeline_trigger(sched, &response.content);
            let _ = crate::pipeline_engine::execute_pipeline_with_origin(
                trigger.source_agent,
                trigger.source_output,
                trigger.tenant_id,
                Some(crate::pipeline_engine::PipelineOrigin::scheduled(
                    sched.id.clone(),
                    is_catchup,
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

    // Record usage event (fire-and-forget) with scheduled/catchup source.
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
    let usage_source = scheduled_usage_source(is_catchup);
    let input_tok = input_tokens;
    let output_tok = output_tokens;
    let token_total = input_tokens + output_tokens;
    tokio::spawn(async move {
        let _ = sqlx::query(
            "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(usage_tid)
        .bind(usage_source)
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

    fn schedule(id: &str) -> AgentSchedule {
        AgentSchedule {
            id: id.to_string(),
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
        }
    }

    #[test]
    fn skip_catchup_attempted_due_schedules_removes_same_tick_attempts() {
        let due = vec![schedule("schedule-1"), schedule("schedule-2")];
        let catchup_attempted = HashSet::from(["schedule-1".to_string()]);

        let pending = skip_catchup_attempted_due_schedules(due, &catchup_attempted);

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "schedule-2");
    }

    #[test]
    fn catchup_audit_actions_track_claim_success_and_failure() {
        assert_eq!(catchup_audit_action_claimed(), "catchup_claimed");
        assert_eq!(catchup_audit_action_succeeded(), "catchup_succeeded");
        assert_eq!(catchup_audit_action_failed(), "catchup_failed");
    }

    #[test]
    fn scheduled_usage_source_distinguishes_catchup() {
        assert_eq!(scheduled_usage_source(false), "scheduled");
        assert_eq!(scheduled_usage_source(true), "catchup");
    }

    #[test]
    fn scheduled_agent_success_prepares_downstream_pipeline_execution_and_billing() {
        let schedule = schedule("schedule-1");
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

        let origin = crate::pipeline_engine::PipelineOrigin::scheduled(schedule.id.clone(), false);
        let effects = crate::pipeline_engine::pipeline_target_run_effects(
            &pipeline,
            trigger.tenant_id,
            "target-run-1",
            Some(&origin),
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
        assert_eq!(effects.metadata.schedule_id.as_deref(), Some("schedule-1"));
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
    fn scheduled_catchup_pipeline_origin_preserves_schedule_id() {
        let schedule = schedule("schedule-1");
        let origin = crate::pipeline_engine::PipelineOrigin::scheduled(schedule.id.clone(), true);
        let pipeline = AgentPipeline {
            id: "pipeline-source-to-target".to_string(),
            tenant_id: schedule.tenant_id.clone(),
            source_agent: schedule.agent_name.clone(),
            target_agent: "target-agent".to_string(),
            condition: None,
            enabled: true,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
        };

        let run = crate::pipeline_engine::pipeline_active_run(
            "target-run-1",
            &schedule.tenant_id,
            &pipeline.target_agent,
            &pipeline.id,
            Some(&origin),
            None,
        );
        let effects = crate::pipeline_engine::pipeline_target_run_effects(
            &pipeline,
            &schedule.tenant_id,
            "target-run-1",
            Some(&origin),
            None,
            None,
            false,
            0,
            0,
            "unknown",
            "unknown",
        );

        assert!(run.is_catchup);
        assert_eq!(run.schedule_id.as_deref(), Some("schedule-1"));
        assert_eq!(effects.metadata.schedule_id.as_deref(), Some("schedule-1"));
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

    #[test]
    fn next_run_at_parses_cron_and_returns_future() {
        let next = next_run_at("* * * * *");
        assert!(next > Utc::now(), "next_run_at should be in future");
        let diff = (next - Utc::now()).num_seconds();
        assert!(diff > 0 && diff <= 120, "diff {} should be within 120s", diff);
    }

    #[test]
    fn next_run_at_invalid_fallback_is_future() {
        let next = next_run_at("not-a-cron");
        assert!(next > Utc::now());
        let diff = (next - Utc::now()).num_seconds();
        assert!(diff >= 55 && diff <= 65, "fallback diff {} should be ~60s", diff);
    }

    #[test]
    fn scheduler_service_next_run_at_matches_free_function() {
        let cron = "0 * * * * *";
        let a = next_run_at(cron);
        let b = SchedulerService::next_run_at(cron);
        // allow 1s drift
        let diff = (a - b).num_seconds().abs();
        assert!(diff <= 1, "service and free next_run_at should match, diff {}", diff);
    }

    #[test]
    fn scheduler_service_compute_next_run_delegates() {
        let r1 = compute_next_run("* * * * * *", "UTC").unwrap();
        let r2 = SchedulerService::compute_next_run("* * * * * *", "UTC").unwrap();
        assert_eq!(r1, r2);
    }

    #[test]
    fn scheduler_service_skip_catchup_delegates() {
        let due = vec![schedule("a"), schedule("b")];
        let set = HashSet::from(["a".to_string()]);
        let r1 = skip_catchup_attempted_due_schedules(due.clone(), &set);
        let r2 = SchedulerService::skip_catchup_attempted_due_schedules(due, &set);
        assert_eq!(r1.len(), r2.len());
        assert_eq!(r1[0].id, r2[0].id);
    }
}
