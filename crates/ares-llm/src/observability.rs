//! Observability callbacks for LLM and tool call logging.
//!
//! Provides a lightweight, dependency-free trait that downstream crates
//! (e.g. `ares` or `ares-agent`) can implement to persist run history.
//!
//! # Design
//!
//! - `ares-llm` defines the trait and record shapes.
//! - Consumers provide an `Arc<dyn ObservabilitySink>` to `ToolCoordinator`
//!   (or `ConfigurableAgent` in `ares-agent`).
//! - The consumer's implementation writes to `run_history` tables via
//!   `ares-store::RunHistoryStore`.

use serde_json::Value;

/// Record of a single LLM call within an agent run.
#[derive(Debug, Clone)]
pub struct LlmCallRecord {
    /// Step index (iteration number) within the run.
    pub step_index: i32,
    /// Provider name (e.g. "openai", "nvidia").
    pub provider: String,
    /// Model name (e.g. "gpt-4o").
    pub model: String,
    /// Number of prompt tokens consumed.
    pub prompt_tokens: i64,
    /// Number of completion tokens consumed.
    pub completion_tokens: i64,
    /// Request latency in milliseconds.
    pub latency_ms: i64,
    /// Status: "success", "error", etc.
    pub status: String,
}

/// Record of a single tool call within an agent run.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    /// Step index (iteration number) within the run.
    pub step_index: i32,
    /// Tool name.
    pub tool_name: String,
    /// Tool type (e.g. "builtin", "mcp", "runtime").
    pub tool_type: String,
    /// Arguments passed to the tool.
    pub arguments: Value,
    /// Result returned by the tool (None if not yet available or failed).
    pub result: Option<Value>,
    /// Execution latency in milliseconds.
    pub latency_ms: i64,
    /// Status: "success", "error", "timeout", etc.
    pub status: String,
}

/// Trait for sinks that receive observability events.
///
/// Implementors should *not* fail the agent run on logging errors.
/// Errors should be logged via `tracing` and swallowed.
#[async_trait::async_trait]
pub trait ObservabilitySink: Send + Sync {
    /// Log an LLM call.
    async fn log_llm_call(&self, record: LlmCallRecord);

    /// Log a tool call.
    async fn log_tool_call(&self, record: ToolCallRecord);
}
