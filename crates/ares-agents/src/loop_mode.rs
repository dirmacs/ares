//! Long-running iteration mode for ares agents.
//!
//! Provides a data model for agents that execute on a fixed interval
//! (cron-like) rather than responding to a single request. Each "beat"
//! runs one unit of work; the agent continues until it hits a halt
//! condition (max iterations, consecutive failures, external stop).
//!
//! Phase 1 (this module): pure data types and configuration. The
//! runtime scheduler, tick dispatcher, and integration with
//! `orchestrator` land in follow-up phases.

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Configuration for a long-running iteration-mode agent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopModeConfig {
    /// Wall-clock interval between iterations, in seconds.
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// Maximum total iterations before the agent halts. `None` = unbounded.
    #[serde(default)]
    pub max_iterations: Option<u64>,
    /// Halt if this many consecutive iterations fail.
    #[serde(default = "default_halt_threshold")]
    pub halt_on_consecutive_failures: u32,
    /// Prompt used when the agent has nothing picked for the current iteration
    /// (anti-idle fallback).
    #[serde(default)]
    pub fallback_prompt: Option<String>,
    /// Whether failed iterations count against `max_iterations`.
    #[serde(default = "default_count_failures")]
    pub count_failed_iterations: bool,
}

fn default_interval_secs() -> u64 {
    180
}
fn default_halt_threshold() -> u32 {
    3
}
fn default_count_failures() -> bool {
    true
}

impl LoopModeConfig {
    /// The interval as a `std::time::Duration` (minimum 1ms for scheduler safety).
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs).max(Duration::from_millis(1))
    }
}

impl Default for LoopModeConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_interval_secs(),
            max_iterations: None,
            halt_on_consecutive_failures: default_halt_threshold(),
            fallback_prompt: None,
            count_failed_iterations: default_count_failures(),
        }
    }
}

/// Runtime state of a long-running iteration-mode agent.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopModeState {
    pub iterations_run: u64,
    pub iterations_succeeded: u64,
    pub iterations_failed: u64,
    pub consecutive_failures: u32,
    pub started_at_epoch_secs: u64,
    pub last_tick_epoch_secs: u64,
}

impl LoopModeState {
    /// Record a successful iteration. Resets the consecutive-failure counter.
    pub fn record_success(&mut self, now_epoch_secs: u64) {
        self.iterations_run += 1;
        self.iterations_succeeded += 1;
        self.consecutive_failures = 0;
        self.last_tick_epoch_secs = now_epoch_secs;
    }

    /// Record a failed iteration. Increments the consecutive-failure counter.
    pub fn record_failure(&mut self, now_epoch_secs: u64) {
        self.iterations_run += 1;
        self.iterations_failed += 1;
        self.consecutive_failures += 1;
        self.last_tick_epoch_secs = now_epoch_secs;
    }

    /// Check whether the state should halt given the provided config.
    pub fn should_halt(&self, config: &LoopModeConfig) -> Option<LoopFinishReason> {
        if self.consecutive_failures >= config.halt_on_consecutive_failures {
            return Some(LoopFinishReason::ConsecutiveFailures);
        }
        if let Some(max) = config.max_iterations {
            let counted = if config.count_failed_iterations {
                self.iterations_run
            } else {
                self.iterations_succeeded
            };
            if counted >= max {
                return Some(LoopFinishReason::MaxIterationsReached);
            }
        }
        None
    }
}

/// Reason a long-running iteration-mode agent stopped.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoopFinishReason {
    /// Hit `max_iterations` from config.
    MaxIterationsReached,
    /// Hit `halt_on_consecutive_failures`.
    ConsecutiveFailures,
    /// External stop signal (user, supervisor, SIGTERM).
    ExternalStop,
    /// Runtime error that the agent could not recover from.
    FatalError,
}

/// Boxed async tick function: returns Ok(()) on success, Err on failure.
pub type TickFn = Box<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

/// Runtime scheduler that drives an iteration-mode agent.
pub struct LoopRunner {
    pub config: LoopModeConfig,
    pub state: LoopModeState,
    stop: Arc<AtomicBool>,
}

impl LoopRunner {
    pub fn new(config: LoopModeConfig) -> Self {
        Self {
            config,
            state: LoopModeState::default(),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    pub async fn run(&mut self, tick: &TickFn) -> LoopFinishReason {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.state.started_at_epoch_secs = now;

        let mut interval = tokio::time::interval(self.config.interval());
        interval.tick().await; // first tick fires immediately

        loop {
            if self.stop.load(Ordering::Relaxed) {
                return LoopFinishReason::ExternalStop;
            }

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            match tick().await {
                Ok(()) => self.state.record_success(now),
                Err(_) => self.state.record_failure(now),
            }

            if let Some(reason) = self.state.should_halt(&self.config) {
                return reason;
            }

            interval.tick().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_sensible() {
        let cfg = LoopModeConfig::default();
        assert_eq!(cfg.interval_secs, 180);
        assert_eq!(cfg.interval(), Duration::from_secs(180));
        assert_eq!(cfg.max_iterations, None);
        assert_eq!(cfg.halt_on_consecutive_failures, 3);
        assert!(cfg.fallback_prompt.is_none());
        assert!(cfg.count_failed_iterations);
    }

    #[test]
    fn config_round_trips_through_toml() {
        let cfg = LoopModeConfig {
            interval_secs: 60,
            max_iterations: Some(100),
            halt_on_consecutive_failures: 5,
            fallback_prompt: Some("expand test coverage".into()),
            count_failed_iterations: false,
        };
        let serialized = toml::to_string(&cfg).expect("serialize");
        let parsed: LoopModeConfig = toml::from_str(&serialized).expect("parse");
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn state_record_success_resets_consecutive_failures() {
        let mut state = LoopModeState::default();
        state.record_failure(10);
        state.record_failure(20);
        assert_eq!(state.consecutive_failures, 2);
        state.record_success(30);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.iterations_run, 3);
        assert_eq!(state.iterations_succeeded, 1);
        assert_eq!(state.iterations_failed, 2);
        assert_eq!(state.last_tick_epoch_secs, 30);
    }

    #[test]
    fn should_halt_on_consecutive_failures() {
        let cfg = LoopModeConfig {
            halt_on_consecutive_failures: 3,
            ..LoopModeConfig::default()
        };
        let mut state = LoopModeState::default();
        state.record_failure(10);
        state.record_failure(20);
        assert_eq!(state.should_halt(&cfg), None);
        state.record_failure(30);
        assert_eq!(
            state.should_halt(&cfg),
            Some(LoopFinishReason::ConsecutiveFailures)
        );
    }

    #[test]
    fn should_halt_on_max_iterations_counting_failures() {
        let cfg = LoopModeConfig {
            max_iterations: Some(2),
            halt_on_consecutive_failures: 999,
            count_failed_iterations: true,
            ..LoopModeConfig::default()
        };
        let mut state = LoopModeState::default();
        state.record_success(10);
        assert_eq!(state.should_halt(&cfg), None);
        state.record_failure(20);
        assert_eq!(
            state.should_halt(&cfg),
            Some(LoopFinishReason::MaxIterationsReached)
        );
    }

    #[test]
    fn should_halt_on_max_iterations_ignoring_failures() {
        let cfg = LoopModeConfig {
            max_iterations: Some(2),
            halt_on_consecutive_failures: 999,
            count_failed_iterations: false,
            ..LoopModeConfig::default()
        };
        let mut state = LoopModeState::default();
        state.record_failure(10);
        state.record_failure(20);
        state.record_failure(30);
        assert_eq!(state.should_halt(&cfg), None);
        state.record_success(40);
        state.record_success(50);
        assert_eq!(
            state.should_halt(&cfg),
            Some(LoopFinishReason::MaxIterationsReached)
        );
    }

    #[test]
    fn unbounded_loop_never_halts_without_failures() {
        let cfg = LoopModeConfig::default();
        let mut state = LoopModeState::default();
        for i in 0..1000 {
            state.record_success(i);
        }
        assert_eq!(state.should_halt(&cfg), None);
    }

    #[test]
    fn finish_reason_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&LoopFinishReason::MaxIterationsReached).unwrap(),
            "\"max_iterations_reached\""
        );
        assert_eq!(
            serde_json::to_string(&LoopFinishReason::ConsecutiveFailures).unwrap(),
            "\"consecutive_failures\""
        );
    }

    #[tokio::test]
    async fn loop_runner_halts_on_max_iterations() {
        let config = LoopModeConfig {
            interval_secs: 0,
            max_iterations: Some(3),
            halt_on_consecutive_failures: 999,
            ..LoopModeConfig::default()
        };
        let mut runner = LoopRunner::new(config);
        let tick: TickFn = Box::new(|| Box::pin(async { Ok(()) }));
        let reason = runner.run(&tick).await;
        assert_eq!(reason, LoopFinishReason::MaxIterationsReached);
        assert_eq!(runner.state.iterations_run, 3);
    }

    #[tokio::test]
    async fn loop_runner_halts_on_consecutive_failures() {
        let config = LoopModeConfig {
            interval_secs: 0,
            max_iterations: None,
            halt_on_consecutive_failures: 2,
            ..LoopModeConfig::default()
        };
        let mut runner = LoopRunner::new(config);
        let tick: TickFn = Box::new(|| Box::pin(async { Err("fail".into()) }));
        let reason = runner.run(&tick).await;
        assert_eq!(reason, LoopFinishReason::ConsecutiveFailures);
        assert_eq!(runner.state.consecutive_failures, 2);
    }

    #[tokio::test]
    async fn loop_runner_external_stop() {
        let config = LoopModeConfig {
            interval_secs: 0,
            max_iterations: None,
            halt_on_consecutive_failures: 999,
            ..LoopModeConfig::default()
        };
        let mut runner = LoopRunner::new(config);
        let stop = runner.stop_handle();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let tick: TickFn = Box::new(|| Box::pin(async { Ok(()) }));
        let reason = runner.run(&tick).await;
        assert_eq!(reason, LoopFinishReason::ExternalStop);
    }
}
