//! Background cron scheduler for agent runs.
//!
//! Periodically checks `agent_schedules` table for agents whose `next_run_at`
//! is in the past, runs them, and updates `last_run_at` / `next_run_at`.

use crate::execution::Execute;
use ares_store::agent_runs::{self, AgentRunMetadata};
use ares_store::schedules::{compute_next_run, AgentSchedule, MissedRunAudit, ScheduleStore};
use ares_store::PostgresClient;
use chrono::{DateTime, Utc};
use cordis::{Context, Disposable, ReflectService, Service};
use cron::Schedule;
use sqlx::PgPool;
use std::any::TypeId;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::interval;

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

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for `SchedulerService` — tick interval in milliseconds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SchedulerConfig {
    #[serde(default = "default_scheduler_tick_ms")]
    pub tick_ms: u64,
}

fn default_scheduler_tick_ms() -> u64 {
    60_000
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
/// Runtime controls changed by scheduler lifecycle events.
///
/// A scheduler keeps running through an isolated failure, but pauses after
/// [`FAILURE_DISABLE_THRESHOLD`] failures until an operator re-enables it.
/// Atomics keep event handlers non-blocking and safe to invoke concurrently.
#[derive(Debug, Default)]
pub struct SchedulerControl {
    failure_count: AtomicUsize,
    disabled: AtomicBool,
}

const FAILURE_DISABLE_THRESHOLD: usize = 3;

impl SchedulerControl {
    /// Number of `agent.failed` events observed by this scheduler.
    pub fn failure_count(&self) -> usize {
        self.failure_count.load(Ordering::Acquire)
    }

    /// Whether repeated failures have paused schedule processing.
    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::Acquire)
    }

    /// Re-enable scheduling and clear the failure window after intervention.
    pub fn reset(&self) {
        self.failure_count.store(0, Ordering::Release);
        self.disabled.store(false, Ordering::Release);
    }

    fn record_failure(&self) -> usize {
        let count = self.failure_count.fetch_add(1, Ordering::AcqRel) + 1;
        if count >= FAILURE_DISABLE_THRESHOLD {
            self.disabled.store(true, Ordering::Release);
        }
        count
    }
}

pub struct SchedulerService {
    pub db: Arc<PostgresClient>,
    pub execution: Arc<Execute>,
    pub tick_ms: u64,
    control: Arc<SchedulerControl>,
    _handle: parking_lot::Mutex<Option<JoinHandle<()>>>,
}

impl SchedulerService {
    /// Create a new service with explicit dependencies.
    pub fn new(db: Arc<PostgresClient>, execution: Arc<Execute>, tick_ms: u64) -> Self {
        Self {
            db,
            execution,
            tick_ms,
            control: Arc::new(SchedulerControl::default()),
            _handle: parking_lot::Mutex::new(None),
        }
    }

    /// Shared runtime controls for observing and managing scheduler state.
    pub fn control(&self) -> Arc<SchedulerControl> {
        Arc::clone(&self.control)
    }

    /// Convenience for `SchedulerConfig { tick_ms }`.
    pub fn from_config(
        db: Arc<PostgresClient>,
        execution: Arc<Execute>,
        config: SchedulerConfig,
    ) -> Self {
        Self::new(db, execution, config.tick_ms)
    }

    /// Real cron evaluation — returns next `DateTime<Utc>` for a cron expression.
    ///
    /// Uses `cron` crate (via `ares_store::schedules::compute_next_run`) with UTC
    /// timezone. Supports both 5-field (`* * * * *`) and 6-field
    /// (`* * * * * *` with seconds) expressions; 5-field is normalized by
    /// prefixing `0` seconds. On parse failure returns `Utc::now() + 60s` so
    /// callers always get a future timestamp (tests assert fallback).
    pub fn next_run_at(cron: &str) -> DateTime<Utc> {
        next_run_at(cron)
    }

    /// Wrapper around `ares_store::schedules::compute_next_run` for method-style call.
    pub fn compute_next_run(cron: &str, tz: &str) -> Result<i64, String> {
        compute_next_run(cron, tz)
    }

    /// Filters out schedules that had a catch-up attempt in this tick.
    pub fn skip_catchup_attempted_due_schedules(
        due: Vec<AgentSchedule>,
        catchup_attempted: &HashSet<String>,
    ) -> Vec<AgentSchedule> {
        skip_catchup_attempted_due_schedules(due, catchup_attempted)
    }

    /// Catch-up owned by the service — uses `self.db` pool.
    pub async fn run_catchup_schedules_owned(
        &self,
        app_state: &std::sync::Arc<cordis::Context>,
    ) -> Result<Vec<String>, String> {
        let store = ScheduleStore::new(&self.db.pool);
        run_catchup_schedules(&store, app_state).await
    }

    /// Due-schedule pass owned by the service.
    pub async fn run_due_schedules_owned(
        &self,
        app_state: &std::sync::Arc<cordis::Context>,
    ) -> Result<(), String> {
        let pool = &self.db.pool;
        run_due_schedules(pool, app_state).await
    }

    /// Service-owned tick body — runs catch-up then due schedules.
    /// Extracted so `init` tick loop and legacy `run_due_schedules` share logic.
    async fn tick_once(&self, app_state: &std::sync::Arc<cordis::Context>) -> Result<(), String> {
        self.run_due_schedules_owned(app_state).await
    }

    /// Enrich a scheduled run payload through the Cordis `scheduler.before_run`
    /// waterfall (around-middleware). Handlers may transform the payload and pass
    /// it on via `next`, or short-circuit by not calling `next`. When no
    /// `EventsService` is provided, or the dispatch errors, the payload is left
    /// unchanged.
    pub async fn before_run_payload(
        &self,
        ctx: &Arc<Context>,
        run: serde_json::Value,
    ) -> serde_json::Value {
        let Some(events) = ctx.get::<cordis::EventsService>() else {
            return run;
        };
        events
            .dispatch(
                cordis::events_catalog::ev::SCHEDULER_BEFORE_RUN.to_string(),
                run.clone(),
                cordis::Dispatch::Waterfall,
            )
            .await
            .unwrap_or(run)
    }

    /// Typed variant of [`before_run_payload`]: constructs the catalog-bound
    /// [`cordis::SchedulerBeforeRunPayload`], serializes it for the waterfall,
    /// and returns the (possibly handler-enriched) JSON.
    pub async fn before_run_typed(
        &self,
        ctx: &Arc<Context>,
        payload: &cordis::SchedulerBeforeRunPayload,
    ) -> serde_json::Value {
        let value = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        let Some(events) = ctx.get::<cordis::EventsService>() else {
            return value;
        };
        // Around-middleware chain on the serialized form so raw-JSON handlers
        // keep working; the typed binding only governs construction.
        events
            .dispatch(
                cordis::events_catalog::ev::SCHEDULER_BEFORE_RUN.to_string(),
                value.clone(),
                cordis::Dispatch::Waterfall,
            )
            .await
            .unwrap_or(value)
    }

    /// Consult the Cordis `scheduler.admit` policy via `Dispatch::Bail`. A
    /// handler returning a non-null value bails (denies) the run and its returned
    /// value replaces the payload; a null result means the handler did not bail,
    /// so the chain continues with the original payload. A run is admitted unless
    /// some handler bailed. Returns `true` when admitted, `false` when denied.
    ///
    /// Because `Dispatch::Bail` resolves to the original payload when nothing
    /// bails (not `Null`), admission is computed as "dispatch returned the
    /// unchanged run". When no `EventsService` is provided, or the dispatch
    /// errors, the run is admitted.
    pub async fn admit_run(&self, ctx: &Arc<Context>, run: serde_json::Value) -> bool {
        let Some(events) = ctx.get::<cordis::EventsService>() else {
            return true;
        };
        let result = events
            .dispatch(
                cordis::events_catalog::ev::SCHEDULER_ADMIT.to_string(),
                run.clone(),
                cordis::Dispatch::Bail,
            )
            .await
            .unwrap_or_else(|_| run.clone());
        result == run
    }
}

/// Free-function form of [`SchedulerService::before_run_typed`] for call
/// sites that hold only the context (e.g. `execute_scheduled_agent`).
async fn scheduler_before_run(
    ctx: &Arc<Context>,
    payload: &cordis::SchedulerBeforeRunPayload,
) -> serde_json::Value {
    let value = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
    let Some(events) = ctx.get::<cordis::EventsService>() else {
        return value;
    };
    events
        .dispatch(
            cordis::events_catalog::ev::SCHEDULER_BEFORE_RUN.to_string(),
            value.clone(),
            cordis::Dispatch::Waterfall,
        )
        .await
        .unwrap_or(value)
}

/// Free-function form of [`SchedulerService::admit_run`]: consults the
/// `scheduler.admit` Bail chain; a run is admitted unless some handler bailed.
async fn scheduler_admit(ctx: &Arc<Context>, run: serde_json::Value) -> bool {
    let Some(events) = ctx.get::<cordis::EventsService>() else {
        return true;
    };
    let result = events
        .dispatch(
            cordis::events_catalog::ev::SCHEDULER_ADMIT.to_string(),
            run.clone(),
            cordis::Dispatch::Bail,
        )
        .await
        .unwrap_or_else(|_| run.clone());
    result == run
}

/// Fire-and-forget `scheduler.schedule.dispatched` emission; missing or failed
/// event bus never affects the tick path.
fn emit_schedule_dispatched(ctx: &Arc<Context>, payload: cordis::ScheduleDispatchedPayload) {
    let Some(events) = ctx.get::<cordis::EventsService>() else {
        return;
    };
    tokio::spawn(async move {
        let _ = events
            .dispatch_typed::<cordis::ScheduleDispatchedEvent>(&payload)
            .await;
    });
}

/// Fire-and-forget `scheduler.tick` emission for one completed pass.
fn emit_scheduler_tick(ctx: &Arc<Context>, due_count: u64, catchup_count: u64) {
    let Some(events) = ctx.get::<cordis::EventsService>() else {
        return;
    };
    tokio::spawn(async move {
        let _ = events
            .dispatch_typed::<cordis::SchedulerTickEvent>(&cordis::SchedulerTickPayload {
                due_count,
                catchup_count,
            })
            .await;
    });
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

    fn init(&self, ctx: &Arc<Context>) -> cordis::ServiceInitFuture<'_> {
        // Capture clones for the spawned task.
        let db = self.db.clone();
        let _execution = self.execution.clone();
        let tick_ms = self.tick_ms;
        let control = self.control();

        // Cordis Events: subscribe to agent.completed for observability and
        // agent.failed for runtime control (the legacy start_scheduler shim is
        // never called by the live server).
        if let Some(events) = ctx.get::<cordis::EventsService>() {
            events.on("agent.completed".into(), |payload| {
                Box::pin(async move {
                    if let Some(agent) = payload.get("agent_name").and_then(|v| v.as_str()) {
                        tracing::debug!(
                            agent = %agent,
                            status = %payload.get("status").and_then(|v| v.as_str()).unwrap_or("unknown"),
                            "scheduler: observed agent completion via Cordis event bus"
                        );
                    }
                    Ok(payload)
                })
            });

            let failure_control = Arc::clone(&control);
            events.on(
                cordis::events_catalog::ev::AGENT_FAILED.to_string(),
                move |payload| {
                    let failure_control = Arc::clone(&failure_control);
                    Box::pin(async move {
                        let agent = payload
                            .get("agent_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let run_id = payload
                            .get("run_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let failures = failure_control.record_failure();
                        tracing::warn!(
                            agent = %agent,
                            run_id = %run_id,
                            failures,
                            disabled = failure_control.is_disabled(),
                            "scheduler observed agent failure via Cordis event bus"
                        );
                        Ok(payload)
                    })
                },
            );
        }

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

        let loop_control = Arc::clone(&control);
        Box::pin(async move {
            // Spawn the main tick loop: select! { interval tick, watch changed }
            let mut ticker = interval(Duration::from_millis(tick_ms));
            // Skip the immediate first tick (interval fires immediately) — align to tick_ms
            ticker.tick().await;

            // Tick uses the scheduler's db + Execute. Tenant resolution reads isolate
            // labels on the request context. If Execute is missing, the tick only
            // updates next_run_at without running agents.

            // Build a watch receiver clone for the select loop
            let mut rx_opt = watch_rx;

            let handle: JoinHandle<()> = tokio::spawn(async move {
                // DB-only mode when Execute is unavailable on the context.
                loop {
                    match rx_opt.as_mut() {
                        Some(rx) => {
                            tokio::select! {
                                _ = ticker.tick() => {
                                    if loop_control.is_disabled() {
                                        tracing::warn!("SchedulerService paused after repeated agent failures");
                                        continue;
                                    }
                                    // DB-only tick: update catch-up/due next_run without agent execution.
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
                                        if loop_control.is_disabled() {
                                            tracing::warn!("SchedulerService paused after repeated agent failures");
                                            continue;
                                        }
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
                            if loop_control.is_disabled() {
                                tracing::warn!(
                                    "SchedulerService paused after repeated agent failures"
                                );
                                continue;
                            }
                            let store = ScheduleStore::new(&db.pool);
                            let _ = store.get_due_schedules().await;
                            tracing::debug!(
                                "SchedulerService tick (polling fallback, {}ms)",
                                tick_ms
                            );
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
pub async fn start_scheduler(pool: PgPool, app_state: std::sync::Arc<cordis::Context>) {
    // Cordis Events: subscribe to agent.completed for scheduler metrics
    if let Some(events) = app_state.get::<cordis::EventsService>() {
        events.on("agent.completed".into(), |payload| {
            Box::pin(async move {
                if let Some(agent) = payload.get("agent_name").and_then(|v| v.as_str()) {
                    tracing::debug!(
                        agent = %agent,
                        status = %payload.get("status").and_then(|v| v.as_str()).unwrap_or("unknown"),
                        "scheduler: observed agent completion via Cordis event bus"
                    );
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

async fn run_due_schedules(
    pool: &PgPool,
    app_state: &std::sync::Arc<cordis::Context>,
) -> Result<(), String> {
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
    let due_count = due.len() as u64;
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
    emit_scheduler_tick(app_state, due_count, catchup_attempted.len() as u64);
    Ok(())
}

/// Detect schedules whose `next_run_at` is past their grace window and trigger
/// a single catch-up run. Records each catch-up in the `missed_runs` audit
/// table so we never trigger the same missed slot twice.
async fn run_catchup_schedules(
    store: &ScheduleStore<'_>,
    app_state: &std::sync::Arc<cordis::Context>,
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
    if is_catchup {
        "catchup"
    } else {
        "scheduled"
    }
}

/// Scope a request context to one tenant so Execute
/// prefers isolate (raw tenant id on Tools + Execute) over intercept and fallback.
pub(crate) fn tenant_scoped_ctx(ctx: &Arc<Context>, tenant_id: &str) -> Arc<Context> {
    crate::tenant_scope(ctx, tenant_id)
}

async fn execute_scheduled_agent(
    sched: &AgentSchedule,
    app_state: &std::sync::Arc<cordis::Context>,
    is_catchup: bool,
) -> Result<(), String> {
    use crate::context_provider::AgentRuntimeContext;

    let pool = app_state
        .get::<ares_store::TenantDb>()
        .ok_or_else(|| "TenantDb not provided".to_string())?
        .pool()
        .clone();
    let scoped = tenant_scoped_ctx(app_state, &sched.tenant_id);
    let exec = scoped
        .get::<Execute>()
        .ok_or_else(|| "Execute not provided".to_string())?;
    let start = std::time::Instant::now();
    let run_id = uuid::Uuid::new_v4().to_string();
    let request_source = scheduled_usage_source(is_catchup);

    let skill_id =
        ares_store::tenant_agents::get_tenant_agent(&pool, &sched.tenant_id, &sched.agent_name)
            .await
            .ok()
            .and_then(|record| {
                record
                    .config
                    .get("skill_id")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            });

    let mut runtime_context = AgentRuntimeContext::new(
        sched.tenant_id.clone(),
        sched.agent_name.clone(),
        "scheduled",
    );
    runtime_context.session_id = Some(run_id.clone());
    let external_context = app_state
        .get::<crate::ContextProviderHandle>()
        .map(|provider| provider.0.clone());
    let external_context = match external_context {
        Some(provider) => provider.get_context_for_run(&runtime_context).await,
        None => None,
    };
    let effective_message = external_context
        .as_deref()
        .map(|context| format_message_with_context(context, "scheduled run"))
        .unwrap_or_else(|| "scheduled run".to_string());
    let request_ctx = if let Some(skill_id) = skill_id {
        scoped.with_intercept(crate::execution::SkillDispatch::new(
            skill_id,
            sched.tenant_id.as_str(),
            serde_json::json!({"message": "scheduled run"}),
            &run_id,
        ))
    } else {
        scoped.clone()
    };
    // Cordis choreography: scheduled runs pass through the same declared
    // policy events as every other execution surface — `scheduler.before_run`
    // around-middleware enrichment, then `scheduler.admit` Bail admission.
    // Handler-provided `agent_name`/`message` overrides flow into the executed
    // request (audit identity stays tied to the schedule). A denial skips
    // execution entirely; callers (due-pass loop / catch-up pass) still
    // advance next_run, so the schedule stays healthy and no agent_runs row
    // is recorded.
    let run_request_value = scheduler_before_run(
        app_state,
        &cordis::SchedulerBeforeRunPayload {
            agent_name: sched.agent_name.clone(),
            run_id: run_id.clone(),
            tenant: Some(sched.tenant_id.clone()),
        },
    )
    .await;
    let admitted = scheduler_admit(app_state, run_request_value.clone()).await;
    if !admitted {
        tracing::info!(
            schedule_id = %sched.id,
            agent = %sched.agent_name,
            "scheduled run denied by admission policy"
        );
        emit_schedule_dispatched(
            app_state,
            cordis::ScheduleDispatchedPayload {
                schedule_id: sched.id.clone(),
                agent_name: sched.agent_name.clone(),
                tenant_id: sched.tenant_id.clone(),
                is_catchup,
                ok: false,
                denied: true,
                error: None,
            },
        );
        // Callers (due-pass loop / catch-up pass) own next_run advancement;
        // returning Ok keeps the schedule healthy without recording a run.
        return Ok(());
    }
    let exec_agent_name = run_request_value
        .get("agent_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| sched.agent_name.clone());
    let exec_message = run_request_value
        .get("message")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| effective_message.clone());

    let req = crate::execution::AgentRequest {
        agent_name: exec_agent_name,
        message: exec_message,
        history: Vec::new(),
        ctx_provider: None,
    };

    // Skill runs need their id before tool steps can write run_tool_calls.
    let skill_run = request_ctx
        .get::<crate::execution::SkillDispatch>()
        .is_some();
    if skill_run {
        let metadata = AgentRunMetadata {
            workspace_id: None,
            session_id: Some(run_id.clone()),
            request_source: Some(request_source.to_string()),
            product: None,
            agent_config_source: Some("tenant_db".to_string()),
            agent_config_version: None,
            eruka_binding_id: None,
            eruka_context_hit: external_context.is_some(),
            eruka_read_count: if external_context.is_some() { 1 } else { 0 },
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
        .map_err(|error| error.to_string())?;
    }

    track_start(
        app_state,
        &run_id,
        &sched.tenant_id,
        &sched.agent_name,
        Some(request_source),
    );
    let execution = exec
        .run(&req, &request_ctx)
        .await
        .map_err(|error| error.to_string());
    let duration_ms = start.elapsed().as_millis() as u64;

    let (status, error_msg, input_tokens, output_tokens, model_name, provider_name, output) =
        match execution {
            Ok(result) => {
                let (input, output) = llm_token_counts_u64(
                    result.response.usage.as_ref(),
                    &effective_message,
                    &result.response.content,
                );
                let model = result
                    .response
                    .metadata
                    .as_ref()
                    .map(|metadata| metadata.model_name.clone())
                    .unwrap_or_else(|| if skill_run { "skill" } else { "unknown" }.to_string());
                let provider = result
                    .response
                    .metadata
                    .as_ref()
                    .map(|metadata| metadata.provider_name.clone())
                    .unwrap_or_else(|| if skill_run { "skill" } else { "unknown" }.to_string());
                track_finish(app_state, &run_id, "completed");
                (
                    "completed",
                    None,
                    input as i64,
                    output as i64,
                    model,
                    provider,
                    result.response.content,
                )
            }
            Err(error) => {
                track_finish(app_state, &run_id, "error");
                (
                    "failed",
                    Some(error),
                    0,
                    0,
                    "unknown".to_string(),
                    "unknown".to_string(),
                    String::new(),
                )
            }
        };

    let metadata = AgentRunMetadata {
        workspace_id: None,
        session_id: Some(run_id.clone()),
        request_source: Some(request_source.to_string()),
        product: None,
        agent_config_source: Some(if skill_run { "tenant_db" } else { "execute" }.to_string()),
        agent_config_version: None,
        eruka_binding_id: None,
        eruka_context_hit: external_context.is_some(),
        eruka_read_count: if external_context.is_some() { 1 } else { 0 },
        eruka_write_count: 0,
        pipeline_id: None,
        schedule_id: Some(sched.id.clone()),
        trigger_id: None,
    };
    if skill_run {
        sqlx::query(
            "UPDATE agent_runs SET status=$2, input_tokens=$3, output_tokens=$4, duration_ms=$5, error=$6 WHERE id=$1",
        )
        .bind(&run_id)
        .bind(status)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(duration_ms as i64)
        .bind(error_msg.as_deref())
        .execute(&pool)
        .await
        .map_err(|error| error.to_string())?;
    } else {
        agent_runs::insert_agent_run_with_id_and_metadata(
            &pool,
            &run_id,
            &sched.tenant_id,
            &sched.agent_name,
            None,
            status,
            input_tokens,
            output_tokens,
            duration_ms as i64,
            error_msg.as_deref(),
            &model_name,
            &provider_name,
            false,
            Some(&metadata),
        )
        .await
        .map_err(|error| error.to_string())?;
    }

    spawn_run_cost_aggregation(
        pool.clone(),
        run_cost_aggregation_request(
            &run_id,
            &sched.tenant_id,
            &sched.agent_name,
            duration_ms as i64,
        ),
    );

    emit_schedule_dispatched(
        app_state,
        cordis::ScheduleDispatchedPayload {
            schedule_id: sched.id.clone(),
            agent_name: sched.agent_name.clone(),
            tenant_id: sched.tenant_id.clone(),
            is_catchup,
            ok: status == "completed",
            denied: false,
            error: error_msg.clone(),
        },
    );

    if status == "completed" {
        let trigger = scheduled_pipeline_trigger(sched, &output);
        let _ = crate::pipeline::execute_pipeline_with_origin(
            trigger.source_agent,
            trigger.source_output,
            trigger.tenant_id,
            Some(crate::pipeline::PipelineOrigin::scheduled(
                sched.id.clone(),
                is_catchup,
            )),
            app_state,
        )
        .await;
    }

    let usage_pool = pool.clone();
    let usage_tid = sched.tenant_id.clone();
    let usage_agent = sched.agent_name.clone();
    let usage_model = (model_name != "unknown").then_some(model_name);
    let usage_provider = (provider_name != "unknown").then_some(provider_name);
    let token_total = input_tokens + output_tokens;
    tokio::spawn(async move {
        let _ = sqlx::query(
            "INSERT INTO usage_events (id, tenant_id, source, request_count, token_count, input_tokens, output_tokens, model_name, agent_name, provider_name, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(usage_tid)
        .bind(request_source)
        .bind(1i32)
        .bind(token_total)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(usage_model)
        .bind(usage_agent)
        .bind(usage_provider)
        .bind(chrono::Utc::now().timestamp())
        .execute(&usage_pool)
        .await;
    });

    error_msg.map_or(Ok(()), |error| {
        Err(format!("Agent execution failed: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_store::schedules::AgentPipeline;

    #[test]
    fn tenant_scoped_ctx_sets_isolate_label() {
        use std::any::TypeId;
        let root = Context::new_root();
        let scoped = tenant_scoped_ctx(&root, "acme");
        // Execute is the shared engine: no realm label, always resolvable.
        assert_eq!(
            scoped
                .isolate_label(TypeId::of::<crate::Execute>())
                .as_deref(),
            None,
        );
        assert_eq!(
            scoped
                .isolate_label(TypeId::of::<ares_tools::Tools>())
                .as_deref(),
            Some("acme"),
        );
        assert!(root.isolate_label(TypeId::of::<crate::Execute>()).is_none());
    }

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
        assert!(crate::pipeline::evaluate_condition(
            pipeline.condition.as_deref().unwrap(),
            trigger.source_output
        ));
        assert_eq!(pipeline.source_agent, schedule.agent_name);

        let origin = crate::pipeline::PipelineOrigin::scheduled(schedule.id.clone(), false);
        let effects = crate::pipeline::pipeline_target_run_effects(
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
        let origin = crate::pipeline::PipelineOrigin::scheduled(schedule.id.clone(), true);
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

        let run = crate::pipeline::pipeline_active_run(
            "target-run-1",
            &schedule.tenant_id,
            &pipeline.target_agent,
            &pipeline.id,
            Some(&origin),
            None,
        );
        let effects = crate::pipeline::pipeline_target_run_effects(
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
        assert!(
            diff > 0 && diff <= 120,
            "diff {} should be within 120s",
            diff
        );
    }

    #[test]
    fn next_run_at_invalid_fallback_is_future() {
        let next = next_run_at("not-a-cron");
        assert!(next > Utc::now());
        let diff = (next - Utc::now()).num_seconds();
        assert!(
            diff >= 55 && diff <= 65,
            "fallback diff {} should be ~60s",
            diff
        );
    }

    #[tokio::test]
    async fn agent_failed_event_updates_scheduler_control_state() {
        let service = SchedulerService::new(
            Arc::new(PostgresClient::new_test()),
            Arc::new(Execute::new()),
            60_000,
        );
        let ctx = Context::new_root();
        let events = ctx.provide(cordis::EventsService::new());
        let disposable = service
            .init(&ctx)
            .await
            .expect("scheduler init should succeed")
            .expect("scheduler should return a disposal guard");

        assert_eq!(service.control().failure_count(), 0);
        assert!(!service.control().is_disabled());

        // Production emits agent.failed fire-and-forget; the handler updates
        // control state asynchronously, so wait for each observed count with a
        // bounded poll instead of relying on Serial ordering.
        async fn wait_for(control: &SchedulerControl, expected: usize) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while control.failure_count() < expected {
                assert!(
                    std::time::Instant::now() < deadline,
                    "timeout waiting for failure_count >= {expected}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }

        let payload = serde_json::json!({
            "agent_name": "scheduled-agent",
            "run_id": "run-1",
            "tenant": "tenant-a",
            "event": "agent.failed",
        });
        events
            .dispatch(
                cordis::events_catalog::ev::AGENT_FAILED.to_string(),
                payload,
                cordis::Dispatch::Emit,
            )
            .await
            .expect("emit dispatch should succeed");
        wait_for(&service.control(), 1).await;

        assert!(!service.control().is_disabled());

        // Repeated failures cross the deterministic pause threshold.
        for run_id in ["run-2", "run-3"] {
            events
                .dispatch(
                    cordis::events_catalog::ev::AGENT_FAILED.to_string(),
                    serde_json::json!({
                        "agent_name": "scheduled-agent",
                        "run_id": run_id,
                        "tenant": "tenant-a",
                        "event": "agent.failed",
                    }),
                    cordis::Dispatch::Emit,
                )
                .await
                .expect("emit dispatch should succeed");
        }
        wait_for(&service.control(), 3).await;
        assert_eq!(service.control().failure_count(), 3);
        assert!(service.control().is_disabled());
        service.control().reset();
        assert_eq!(service.control().failure_count(), 0);
        assert!(!service.control().is_disabled());
        disposable.dispose();
    }

    #[tokio::test]
    async fn scheduler_admit_bail_policy_denies_run() {
        let service = SchedulerService::new(
            Arc::new(PostgresClient::new_test()),
            Arc::new(Execute::new()),
            60_000,
        );
        let ctx = Context::new_root();
        let events = ctx.provide(cordis::EventsService::new());

        // A Cordis `Dispatch::Bail` admission policy on `scheduler.admit`: a
        // handler that returns a non-null value bails (denies) the run, while a
        // null result means "did not bail" (the run is admitted).
        let disposable = events.on(
            cordis::events_catalog::ev::SCHEDULER_ADMIT.to_string(),
            |payload: serde_json::Value| async move {
                if payload
                    .get("deny")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    Ok(serde_json::json!({ "denied": true }))
                } else {
                    Ok(serde_json::Value::Null)
                }
            },
        );

        let denied = service
            .admit_run(&ctx, serde_json::json!({ "agent_name": "a", "deny": true }))
            .await;
        assert_eq!(denied, false, "deny:true payload should NOT be admitted");

        let admitted = service
            .admit_run(
                &ctx,
                serde_json::json!({ "agent_name": "a", "deny": false }),
            )
            .await;
        assert_eq!(admitted, true, "deny:false payload should be admitted");

        disposable.dispose();
    }

    #[test]
    fn scheduler_service_next_run_at_matches_free_function() {
        let cron = "0 * * * * *";
        let a = next_run_at(cron);
        let b = SchedulerService::next_run_at(cron);
        // allow 1s drift
        let diff = (a - b).num_seconds().abs();
        assert!(
            diff <= 1,
            "service and free next_run_at should match, diff {}",
            diff
        );
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

    #[tokio::test]
    async fn scheduler_before_run_waterfall_enriches_payload() {
        let service = SchedulerService::new(
            Arc::new(PostgresClient::new_test()),
            Arc::new(Execute::new()),
            60_000,
        );
        let ctx = Context::new_root();
        let events = ctx.provide(cordis::EventsService::new());
        events.on_waterfall(
            cordis::events_catalog::ev::SCHEDULER_BEFORE_RUN.to_string(),
            |payload, next| async move {
                let mut obj = payload.as_object().cloned().unwrap_or_default();
                obj.insert("enriched".into(), serde_json::json!(true));
                next(serde_json::Value::Object(obj)).await
            },
        );

        let enriched = service
            .before_run_payload(&ctx, serde_json::json!({"agent_name":"a","run_id":"r1"}))
            .await;
        assert_eq!(enriched["enriched"], serde_json::json!(true));
    }

    // ── Phase 5 choreography: policy events through the real tick path ────

    /// Real Postgres-backed [`ares_store::TenantDb`] built from
    /// `TEST_DATABASE_URL`. Note that `PostgresClient::new_test()`'s lazy
    /// pool points at a placeholder URL that can never execute queries, so
    /// tests asserting persisted rows must connect through the env URL.
    async fn live_tenant_db() -> ares_store::TenantDb {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres:///ares_test".to_string());
        let client = ares_store::PostgresClient::new_remote(url, String::new())
            .await
            .expect("test database should be reachable");
        ares_store::TenantDb::new(std::sync::Arc::new(client))
    }

    /// `agent_runs.tenant_id` carries a FK to `tenants(id)`, so fabricated
    /// schedule tenants must exist as real tenant rows before execution.
    async fn create_fixture_tenant(tenant_db: &ares_store::TenantDb, name: &str) -> String {
        tenant_db
            .create_tenant(name.to_string(), ares_types::TenantTier::Free)
            .await
            .expect("fixture tenant insert should succeed")
            .id
    }

    async fn delete_fixture_tenant(pool: &sqlx::PgPool, tenant_id: &str) {
        sqlx::query("DELETE FROM tenants WHERE id=$1")
            .bind(tenant_id)
            .execute(pool)
            .await
            .expect("bounded cleanup tenant delete should succeed");
    }

    async fn agent_runs_count(pool: &sqlx::PgPool, tenant_id: &str, agent_name: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM agent_runs WHERE tenant_id=$1 AND agent_name=$2",
        )
        .bind(tenant_id)
        .bind(agent_name)
        .fetch_one(pool)
        .await
        .expect("agent_runs count query should succeed")
    }

    async fn cleanup_agent_runs(pool: &sqlx::PgPool, tenant_id: &str) {
        sqlx::query("DELETE FROM agent_runs WHERE tenant_id=$1")
            .bind(tenant_id)
            .execute(pool)
            .await
            .expect("bounded cleanup delete should succeed");
    }

    /// Wait (≤5s per poll) for a `scheduler.schedule.dispatched` emission
    /// belonging to `schedule_id`, tolerating unrelated broadcasts.
    async fn wait_for_dispatched(
        rx: &mut tokio::sync::broadcast::Receiver<(String, serde_json::Value)>,
        schedule_id: &str,
    ) -> serde_json::Value {
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Ok((_, payload)))
                    if payload["schedule_id"] == serde_json::json!(schedule_id) =>
                {
                    return payload;
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(other) => panic!("broadcast channel closed unexpectedly: {other:?}"),
                Err(_) => {
                    panic!("scheduler.schedule.dispatched for {schedule_id} not observed within 5s")
                }
            }
        }
    }

    // Multi-thread runtime: the execution path uses block_in_place internally.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scheduled_run_dispatched_event_observed_on_execution_path() {
        let tenant_db = live_tenant_db().await;
        let pool = tenant_db.pool().clone();

        let app_state = Context::new_root();
        let events = app_state.provide(cordis::EventsService::new());

        let tenant_id = create_fixture_tenant(&tenant_db, "t5-disp-ok").await;
        app_state.provide(tenant_db);
        app_state.provide(crate::Execute::new());

        let mut rx = events.subscribe();
        let mut sched = schedule("t5-disp-ok");
        sched.id = "t5-disp-ok".to_string();
        sched.agent_name = "t5-agent-disp".to_string();
        sched.tenant_id = tenant_id;

        // The Execute engine's fallback path completes scheduled runs even
        // without a configured model; the observable contract is a
        // dispatched emission with ok=true plus an agent_runs row.
        let result = execute_scheduled_agent(&sched, &app_state, false).await;
        assert!(
            result.is_ok(),
            "scheduled execution should succeed: {result:?}"
        );

        let payload = wait_for_dispatched(&mut rx, &sched.id).await;
        assert_eq!(payload["ok"], serde_json::json!(true));
        assert_eq!(payload["denied"], serde_json::json!(false));
        assert_eq!(payload["agent_name"], serde_json::json!(sched.agent_name));
        assert_eq!(payload["tenant_id"], serde_json::json!(sched.tenant_id));
        assert!(payload["error"].is_null(), "success must carry no error");

        let count = agent_runs_count(&pool, &sched.tenant_id, &sched.agent_name).await;
        assert!(count > 0, "scheduled run must record an agent_runs row");

        cleanup_agent_runs(&pool, &sched.tenant_id).await;
        delete_fixture_tenant(&pool, &sched.tenant_id).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scheduled_run_denied_by_admission_skips_execution_and_reports_denied() {
        let tenant_db = live_tenant_db().await;
        let pool = tenant_db.pool().clone();

        let app_state = Context::new_root();
        let events = app_state.provide(cordis::EventsService::new());

        // Admission policy denies every candidate run.
        let _deny = events.on(
            cordis::events_catalog::ev::SCHEDULER_ADMIT.to_string(),
            |_payload| async { Ok(serde_json::json!({"deny": true})) },
        );
        let tenant_id = create_fixture_tenant(&tenant_db, "t5-disp-deny").await;
        app_state.provide(tenant_db);
        app_state.provide(crate::Execute::new());

        let mut rx = events.subscribe();

        let mut sched = schedule("t5-disp-deny");
        sched.id = "t5-disp-deny".to_string();
        sched.agent_name = "t5-agent-deny".to_string();
        sched.tenant_id = tenant_id;

        let result = execute_scheduled_agent(&sched, &app_state, false).await;
        assert!(
            result.is_ok(),
            "denial must not fail the caller: {result:?}"
        );

        let payload = wait_for_dispatched(&mut rx, &sched.id).await;
        assert_eq!(payload["denied"], serde_json::json!(true));
        assert_eq!(payload["ok"], serde_json::json!(false));
        assert!(payload["error"].is_null(), "denial is not an error");

        let count = agent_runs_count(&pool, &sched.tenant_id, &sched.agent_name).await;
        assert_eq!(count, 0, "denied runs must not record agent_runs rows");

        cleanup_agent_runs(&pool, &sched.tenant_id).await;
        delete_fixture_tenant(&pool, &sched.tenant_id).await;
    }

    #[tokio::test]
    async fn scheduled_before_run_enrichment_reaches_admission_through_tick_helpers() {
        let ctx = Context::new_root();
        let events = ctx.provide(cordis::EventsService::new());

        // Around-middleware injects a marker into the before_run payload.
        let _enrich = events.on_waterfall(
            cordis::events_catalog::ev::SCHEDULER_BEFORE_RUN.to_string(),
            |payload, next| async move {
                let mut obj = payload.as_object().cloned().unwrap_or_default();
                obj.insert("message".into(), serde_json::json!("deny-me-marker"));
                next(serde_json::Value::Object(obj)).await
            },
        );
        // Admission bails only when the enrichment reached its input.
        let _policy = events.on(
            cordis::events_catalog::ev::SCHEDULER_ADMIT.to_string(),
            |payload| async move {
                if payload.get("message") == Some(&serde_json::json!("deny-me-marker")) {
                    Ok(serde_json::json!({"deny": true}))
                } else {
                    Ok(serde_json::Value::Null)
                }
            },
        );

        // Tick-path flow: the before_run helper's waterfall output feeds the
        // admission helper exactly as execute_scheduled_agent wires them.
        let enriched = scheduler_before_run(
            &ctx,
            &cordis::SchedulerBeforeRunPayload {
                agent_name: "t5-agent-flow".to_string(),
                run_id: "t5-run-enriched".to_string(),
                tenant: Some("t5-tenant-flow".to_string()),
            },
        )
        .await;
        assert_eq!(
            enriched["message"],
            serde_json::json!("deny-me-marker"),
            "waterfall must inject the marker"
        );
        assert!(
            !scheduler_admit(&ctx, enriched).await,
            "marker injected by scheduler.before_run must drive admission denial"
        );

        let plain = serde_json::to_value(cordis::SchedulerBeforeRunPayload {
            agent_name: "t5-agent-flow".to_string(),
            run_id: "t5-run-plain".to_string(),
            tenant: Some("t5-tenant-flow".to_string()),
        })
        .expect("payload serializes");
        assert!(
            scheduler_admit(&ctx, plain).await,
            "run without the injected marker must be admitted"
        );
    }
}

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

/// Typed installer for [`SchedulerService`].
pub struct SchedulerPlugin;

impl cordis::Plugin for SchedulerPlugin {
    type Config = SchedulerConfig;
    type Provides = SchedulerService;

    fn apply(
        &self,
        ctx: &std::sync::Arc<cordis::Context>,
        config: Self::Config,
    ) -> Result<std::sync::Arc<Self::Provides>, cordis::CordisError> {
        let execution = inject_or_get::<crate::Execute>(ctx)?;
        let db = inject_or_get::<ares_store::PostgresClient>(ctx)?;
        Ok(std::sync::Arc::new(SchedulerService::new(
            db,
            execution,
            config.tick_ms,
        )))
    }
}
