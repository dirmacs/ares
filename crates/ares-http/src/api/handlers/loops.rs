//! HTTP handlers for loop-mode agent lifecycle management.
//!
//! Exposes three endpoints:
//!
//! - `POST /loops/start`   — start a new LoopRunner for a named agent
//! - `GET  /loops`         — list all registered loops with state summaries
//! - `DELETE /loops/{id}`  — signal a running loop to stop

use crate::HttpError;
use crate::Result;
use ares_agent::loop_mode::{LoopFinishReason, LoopModeConfig, LoopModeState, LoopRunner};
use ares_types::types::AppError;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use cordis::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Registry types
// ---------------------------------------------------------------------------

/// In-memory entry for a single running (or finished) loop.
pub struct LoopEntry {
    pub id: String,
    pub agent_name: String,
    pub config: LoopModeConfig,
    pub state: LoopModeState,
    pub stop: Arc<AtomicBool>,
    pub started_at: u64,
    pub finish_reason: Option<LoopFinishReason>,
}

/// Thread-safe registry of all loop entries.
#[derive(Clone, Default)]
pub struct LoopRegistry {
    pub(crate) entries: Arc<Mutex<HashMap<String, LoopEntry>>>,
}

impl LoopRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a loop entry. Returns `true` when the id was not already present.
    pub async fn insert(&self, entry: LoopEntry) -> bool {
        let mut entries = self.entries.lock().await;
        entries.insert(entry.id.clone(), entry).is_none()
    }

    /// List all loops as summaries, newest first.
    pub async fn list(&self) -> Vec<LoopSummary> {
        let entries = self.entries.lock().await;
        let mut list: Vec<LoopSummary> = entries.values().map(Self::entry_to_summary).collect();
        list.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        list
    }

    /// Set the stop flag for `loop_id`. Returns `true` if the loop was found.
    pub async fn stop(&self, loop_id: &str) -> bool {
        let entries = self.entries.lock().await;
        if let Some(e) = entries.get(loop_id) {
            e.stop.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Update the latest state snapshot for `loop_id`. Returns `false` when unknown.
    pub async fn update_state(&self, loop_id: &str, state: LoopModeState) -> bool {
        let mut entries = self.entries.lock().await;
        if let Some(e) = entries.get_mut(loop_id) {
            e.state = state;
            true
        } else {
            false
        }
    }

    /// Whether the loop is still running. Returns `None` when `loop_id` is unknown.
    pub async fn running_flag_for(&self, loop_id: &str) -> Option<bool> {
        let entries = self.entries.lock().await;
        entries
            .get(loop_id)
            .map(|e| e.finish_reason.is_none() && !e.stop.load(Ordering::Relaxed))
    }

    fn entry_to_summary(e: &LoopEntry) -> LoopSummary {
        LoopSummary {
            id: e.id.clone(),
            agent_name: e.agent_name.clone(),
            config: e.config.clone(),
            state: e.state.clone(),
            started_at: e.started_at,
            finish_reason: e.finish_reason.clone(),
            running: e.finish_reason.is_none() && !e.stop.load(Ordering::Relaxed),
        }
    }
}

impl cordis::Service for LoopRegistry {
    fn name(&self) -> &'static str {
        "loop_registry"
    }
    fn init(&self, _ctx: &std::sync::Arc<cordis::Context>) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StartLoopRequest {
    /// Name of the agent (tenant agent name) to tick.
    pub agent: String,
    /// Loop scheduling and halt configuration.
    #[serde(default)]
    pub config: LoopModeConfig,
    /// Prompt passed to the agent on every tick.
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StartLoopResponse {
    pub id: String,
}

/// JSON-serialisable summary of a loop entry (omits the non-serialisable stop handle).
#[derive(Debug, Serialize, Deserialize)]
pub struct LoopSummary {
    pub id: String,
    pub agent_name: String,
    pub config: LoopModeConfig,
    pub state: LoopModeState,
    pub started_at: u64,
    pub finish_reason: Option<LoopFinishReason>,
    /// True while the loop task is still running (no finish_reason yet).
    pub running: bool,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /loops/start — spawn a new LoopRunner task and return its ID.
pub async fn start_loop(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<StartLoopRequest>,
) -> Result<Json<StartLoopResponse>> {
    let id = Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Build the entry and grab the stop handle before moving into the task.
    let mut runner = LoopRunner::new(req.config.clone());
    let stop_handle = runner.stop_handle();

    let entry = LoopEntry {
        id: id.clone(),
        agent_name: req.agent.clone(),
        config: req.config.clone(),
        state: LoopModeState::default(),
        stop: stop_handle.clone(),
        started_at: now,
        finish_reason: None,
    };

    ctx.get::<LoopRegistry>()
        .expect("not provided")
        .insert(entry)
        .await;

    // Clone handles needed by the background task.
    let registry = ctx.get::<LoopRegistry>().expect("not provided").clone();
    let loop_id = id.clone();
    let agent_name = req.agent.clone();
    let prompt = req.prompt.clone();

    tokio::spawn(async move {
        // No-op tick: logs the agent name and prompt, then returns Ok.
        // Future phases will call agent.execute() here once the orchestrator
        // is wired up to LoopRunner.
        let tick = build_tick(agent_name, prompt);
        let state_registry = registry.clone();
        let state_loop_id = loop_id.clone();
        let finish_reason = runner
            .run_with_state_observer(&tick, move |state| {
                let registry = state_registry.clone();
                let loop_id = state_loop_id.clone();
                async move {
                    registry.update_state(&loop_id, state).await;
                }
            })
            .await;

        // Write back the final state and finish reason.
        let mut entries = registry.entries.lock().await;
        if let Some(e) = entries.get_mut(&loop_id) {
            e.state = runner.state.clone();
            e.finish_reason = Some(finish_reason);
        }
    });

    Ok(Json(StartLoopResponse { id }))
}

/// GET /loops — list all loops with their current state summaries.
pub async fn list_loops(State(ctx): State<Arc<Context>>) -> Json<Vec<LoopSummary>> {
    Json(
        ctx.get::<LoopRegistry>()
            .expect("not provided")
            .list()
            .await,
    )
}

/// DELETE /loops/{id} — set the stop flag; the runner will halt after the current tick.
pub async fn stop_loop(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    if ctx
        .get::<LoopRegistry>()
        .expect("not provided")
        .stop(&id)
        .await
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(HttpError::from(AppError::NotFound(
            format!("Loop '{}' not found", id).into(),
        )))
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build a no-op tick function that logs agent name and prompt.
/// Returns Ok(()) immediately — real agent dispatch lands in a future phase.
fn build_tick(agent_name: String, prompt: String) -> ares_agent::loop_mode::TickFn {
    Box::new(move || {
        let agent = agent_name.clone();
        let p = prompt.clone();
        Box::pin(async move {
            tracing::debug!(agent = %agent, prompt = %p, "loop tick (no-op)");
            Ok(())
        })
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ares_agent::loop_mode::{LoopFinishReason, LoopModeConfig, LoopModeState};

    fn dummy_entry(id: &str, started_at: u64) -> LoopEntry {
        LoopEntry {
            id: id.to_string(),
            agent_name: "test-agent".to_string(),
            config: LoopModeConfig::default(),
            state: LoopModeState::default(),
            stop: Arc::new(AtomicBool::new(false)),
            started_at,
            finish_reason: None,
        }
    }

    #[tokio::test]
    async fn registry_new_starts_empty() {
        let registry = LoopRegistry::new();
        assert!(registry.list().await.is_empty());
    }

    #[tokio::test]
    async fn registry_insert_and_list() {
        let registry = LoopRegistry::new();
        assert!(registry.insert(dummy_entry("loop-1", 2)).await);
        assert!(registry.insert(dummy_entry("loop-2", 1)).await);

        let list = registry.list().await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "loop-1");
        assert_eq!(list[1].id, "loop-2");
    }

    #[tokio::test]
    async fn registry_insert_returns_false_for_duplicate_id() {
        let registry = LoopRegistry::new();
        assert!(registry.insert(dummy_entry("dup", 1)).await);
        assert!(!registry.insert(dummy_entry("dup", 2)).await);
        assert_eq!(registry.list().await.len(), 1);
    }

    #[tokio::test]
    async fn registry_stop_sets_flag() {
        let registry = LoopRegistry::new();
        let entry = dummy_entry("loop-stop", 1);
        let stop = entry.stop.clone();
        registry.insert(entry).await;

        assert!(!stop.load(Ordering::Relaxed));
        assert!(registry.stop("loop-stop").await);
        assert!(stop.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn registry_stop_returns_false_for_missing_id() {
        let registry = LoopRegistry::new();
        assert!(!registry.stop("missing").await);
    }

    #[tokio::test]
    async fn registry_finish_reason_transition() {
        let registry = LoopRegistry::new();
        registry.insert(dummy_entry("loop-fin", 1)).await;

        {
            let mut entries = registry.entries.lock().await;
            let e = entries.get_mut("loop-fin").unwrap();
            e.finish_reason = Some(LoopFinishReason::MaxIterationsReached);
        }

        let entries = registry.entries.lock().await;
        assert_eq!(
            entries.get("loop-fin").unwrap().finish_reason,
            Some(LoopFinishReason::MaxIterationsReached)
        );
    }

    #[tokio::test]
    async fn registry_update_state_refreshes_listed_snapshot() {
        let registry = LoopRegistry::new();
        registry.insert(dummy_entry("loop-state", 1)).await;

        let state = LoopModeState {
            iterations_run: 3,
            iterations_succeeded: 2,
            iterations_failed: 1,
            consecutive_failures: 1,
            started_at_epoch_secs: 100,
            last_tick_epoch_secs: 130,
        };
        assert!(registry.update_state("loop-state", state.clone()).await);
        assert!(!registry.update_state("missing", state.clone()).await);

        let list = registry.list().await;
        let entry = list.iter().find(|l| l.id == "loop-state").expect("entry");
        assert_eq!(entry.state, state);
    }

    #[tokio::test]
    async fn registry_running_flag_reflects_state() {
        let registry = LoopRegistry::new();
        let entry = dummy_entry("loop-run", 1);
        let stop = entry.stop.clone();
        registry.insert(entry).await;

        assert_eq!(registry.running_flag_for("loop-run").await, Some(true));

        stop.store(true, Ordering::Relaxed);
        assert_eq!(registry.running_flag_for("loop-run").await, Some(false));
        assert_eq!(registry.running_flag_for("missing").await, None);
    }

    #[tokio::test]
    async fn registry_list_marks_stopped_loop_not_running() {
        let registry = LoopRegistry::new();
        let entry = dummy_entry("loop-run", 1);
        let stop = entry.stop.clone();
        registry.insert(entry).await;

        stop.store(true, Ordering::Relaxed);
        let list = registry.list().await;
        let entry = list.iter().find(|l| l.id == "loop-run").expect("entry");
        assert!(!entry.running);
    }

    #[test]
    fn start_loop_request_deserializes_defaults() {
        let req: StartLoopRequest = serde_json::from_str(r#"{"agent":"support"}"#).unwrap();
        assert_eq!(req.agent, "support");
        assert_eq!(req.config, LoopModeConfig::default());
        assert!(req.prompt.is_empty());
    }

    #[test]
    fn start_loop_response_roundtrip() {
        let resp = StartLoopResponse {
            id: "loop-abc".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: StartLoopResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "loop-abc");
    }

    #[test]
    fn loop_summary_serializes_running_flag() {
        let summary = LoopSummary {
            id: "id-1".into(),
            agent_name: "agent".into(),
            config: LoopModeConfig::default(),
            state: LoopModeState::default(),
            started_at: 42,
            finish_reason: None,
            running: true,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"running\":true"));
        let back: LoopSummary = serde_json::from_str(&json).unwrap();
        assert!(back.running);
        assert_eq!(back.started_at, 42);
    }

    #[test]
    fn loop_mode_config_roundtrip() {
        let config = LoopModeConfig {
            interval_secs: 30,
            max_iterations: Some(5),
            halt_on_consecutive_failures: 2,
            fallback_prompt: Some("idle".into()),
            count_failed_iterations: false,
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: LoopModeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, config);
    }

    #[test]
    fn loop_mode_state_roundtrip() {
        let state = LoopModeState {
            iterations_run: 3,
            iterations_succeeded: 2,
            iterations_failed: 1,
            consecutive_failures: 1,
            started_at_epoch_secs: 100,
            last_tick_epoch_secs: 200,
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: LoopModeState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
    }

    #[test]
    fn loop_finish_reason_roundtrip() {
        for reason in [
            LoopFinishReason::MaxIterationsReached,
            LoopFinishReason::ConsecutiveFailures,
            LoopFinishReason::ExternalStop,
            LoopFinishReason::FatalError,
        ] {
            let json = serde_json::to_string(&reason).unwrap();
            let back: LoopFinishReason = serde_json::from_str(&json).unwrap();
            assert_eq!(back, reason);
        }
    }

    #[test]
    fn loop_mode_state_record_success_resets_failures() {
        let mut state = LoopModeState::default();
        state.record_failure(10);
        state.record_success(20);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.iterations_succeeded, 1);
        assert_eq!(state.last_tick_epoch_secs, 20);
    }

    #[test]
    fn loop_mode_state_should_halt_on_max_iterations() {
        let config = LoopModeConfig {
            max_iterations: Some(2),
            count_failed_iterations: true,
            ..LoopModeConfig::default()
        };
        let mut state = LoopModeState::default();
        state.record_success(1);
        assert!(state.should_halt(&config).is_none());
        state.record_success(2);
        assert_eq!(
            state.should_halt(&config),
            Some(LoopFinishReason::MaxIterationsReached)
        );
    }

    #[tokio::test]
    async fn build_tick_executes_successfully() {
        let tick = build_tick("agent".into(), "prompt".into());
        assert!(tick().await.is_ok());
    }

    #[test]
    fn loop_registry_readable_via_cordis() {
        use cordis::Service;
        let ctx = cordis::Context::new_root();
        ctx.provide(LoopRegistry::new());
        let got = ctx.get::<LoopRegistry>().expect("provided");
        assert_eq!(got.name(), "loop_registry");
        assert!(got.check());
    }
}
