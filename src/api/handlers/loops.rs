//! HTTP handlers for loop-mode agent lifecycle management.
//!
//! Exposes three endpoints:
//!
//! - `POST /loops/start`   — start a new LoopRunner for a named agent
//! - `GET  /loops`         — list all registered loops with state summaries
//! - `DELETE /loops/{id}`  — signal a running loop to stop

use crate::agents::loop_mode::{LoopFinishReason, LoopModeConfig, LoopModeState, LoopRunner};
use crate::types::{AppError, Result};
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
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
    entries: Arc<Mutex<HashMap<String, LoopEntry>>>,
}

impl LoopRegistry {
    pub fn new() -> Self {
        Self::default()
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

#[derive(Debug, Serialize)]
pub struct StartLoopResponse {
    pub id: String,
}

/// JSON-serialisable summary of a loop entry (omits the non-serialisable stop handle).
#[derive(Debug, Serialize)]
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
    State(state): State<AppState>,
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

    state.loop_registry.entries.lock().await.insert(id.clone(), entry);

    // Clone handles needed by the background task.
    let registry = state.loop_registry.clone();
    let loop_id = id.clone();
    let agent_name = req.agent.clone();
    let prompt = req.prompt.clone();

    tokio::spawn(async move {
        // No-op tick: logs the agent name and prompt, then returns Ok.
        // Future phases will call agent.execute() here once the orchestrator
        // is wired up to LoopRunner.
        let tick = build_tick(agent_name, prompt);
        let finish_reason = runner.run(&tick).await;

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
pub async fn list_loops(State(state): State<AppState>) -> Json<Vec<LoopSummary>> {
    let entries = state.loop_registry.entries.lock().await;
    let mut list: Vec<LoopSummary> = entries
        .values()
        .map(|e| LoopSummary {
            id: e.id.clone(),
            agent_name: e.agent_name.clone(),
            config: e.config.clone(),
            state: e.state.clone(),
            started_at: e.started_at,
            finish_reason: e.finish_reason.clone(),
            running: e.finish_reason.is_none() && !e.stop.load(Ordering::Relaxed),
        })
        .collect();
    // Newest first.
    list.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    Json(list)
}

/// DELETE /loops/{id} — set the stop flag; the runner will halt after the current tick.
pub async fn stop_loop(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let entries = state.loop_registry.entries.lock().await;
    match entries.get(&id) {
        Some(e) => {
            e.stop.store(true, Ordering::Relaxed);
            Ok(StatusCode::NO_CONTENT)
        }
        None => Err(AppError::NotFound(format!("Loop '{}' not found", id))),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build a no-op tick function that logs agent name and prompt.
/// Returns Ok(()) immediately — real agent dispatch lands in a future phase.
fn build_tick(
    agent_name: String,
    prompt: String,
) -> crate::agents::loop_mode::TickFn {
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
    use crate::agents::loop_mode::{LoopFinishReason, LoopModeConfig, LoopModeState};

    // Helper: insert a dummy entry into a registry.
    async fn insert_dummy(registry: &LoopRegistry, id: &str) -> Arc<AtomicBool> {
        let stop = Arc::new(AtomicBool::new(false));
        let entry = LoopEntry {
            id: id.to_string(),
            agent_name: "test-agent".to_string(),
            config: LoopModeConfig::default(),
            state: LoopModeState::default(),
            stop: stop.clone(),
            started_at: 1_000_000,
            finish_reason: None,
        };
        registry.entries.lock().await.insert(id.to_string(), entry);
        stop
    }

    #[tokio::test]
    async fn registry_insert_and_list() {
        let registry = LoopRegistry::new();
        insert_dummy(&registry, "loop-1").await;
        insert_dummy(&registry, "loop-2").await;

        let entries = registry.entries.lock().await;
        assert_eq!(entries.len(), 2);
        assert!(entries.contains_key("loop-1"));
        assert!(entries.contains_key("loop-2"));
    }

    #[tokio::test]
    async fn registry_stop_sets_flag() {
        let registry = LoopRegistry::new();
        let stop = insert_dummy(&registry, "loop-stop").await;

        assert!(!stop.load(Ordering::Relaxed));
        {
            let entries = registry.entries.lock().await;
            entries
                .get("loop-stop")
                .unwrap()
                .stop
                .store(true, Ordering::Relaxed);
        }
        assert!(stop.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn registry_finish_reason_transition() {
        let registry = LoopRegistry::new();
        insert_dummy(&registry, "loop-fin").await;

        // Simulate loop completing.
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
    async fn registry_running_flag_reflects_state() {
        let registry = LoopRegistry::new();
        let stop = insert_dummy(&registry, "loop-run").await;

        // Should be running.
        {
            let entries = registry.entries.lock().await;
            let e = entries.get("loop-run").unwrap();
            let running = e.finish_reason.is_none() && !e.stop.load(Ordering::Relaxed);
            assert!(running);
        }

        // After stop flag is set, running = false.
        stop.store(true, Ordering::Relaxed);
        {
            let entries = registry.entries.lock().await;
            let e = entries.get("loop-run").unwrap();
            let running = e.finish_reason.is_none() && !e.stop.load(Ordering::Relaxed);
            assert!(!running);
        }
    }
}
