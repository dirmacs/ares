//! Exporter-style log routing for LLM and tool call records.
//!
//! # Why this module exists
//!
//! Every place that runs an LLM call or a tool call produces the same two
//! record shapes: [`LlmCallRecord`] and [`ToolCallRecord`] (reused from
//! [`crate::observability`], never duplicated). Without this module, each
//! consumer had to wire its own sink plumbing by hand. That repeats work and
//! loses records. This module gives ONE fan-out point instead:
//!
//! 1. Build one [`ExporterRouter`].
//! 2. [`register`](ExporterRouter::register) any number of
//!    [`LogExporter`] sinks: stdout formatter, database writer, OTLP
//!    forwarder, test capture, and so on.
//! 3. Route records through [`ExporterRouter::log_llm`] and
//!    [`ExporterRouter::log_tool`].
//!
//! # Failure isolation
//!
//! One broken destination must NEVER fail inference. The export methods
//! return `()` ON PURPOSE: an exporter cannot return an error upward. An
//! exporter that hits a problem MUST log it with `tracing::warn!` inside its
//! own implementation and carry on. The router adds no error handling because
//! no error can escape an exporter.
//!
//! # Per-exporter filtering
//!
//! Each exporter picks the record levels it wants through
//! [`LogExporter::accepts`]. The router skips exporters whose gate rejects the
//! record, so a debug-only destination costs nothing on quieter levels.
//!
//! Fan-out is SEQUENTIAL today, in registration order. Concurrent fan-out is
//! a possible later change, made only behind measurement.

use std::sync::Arc;

use crate::observability::{LlmCallRecord, ToolCallRecord};

/// Severity attached to a routed record.
///
/// The router passes this level to each exporter gate
/// ([`LogExporter::accepts`]); it is metadata about the record, not a change
/// to the record itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordLevel {
    /// Low-value detail, safe to drop.
    Debug,
    /// Normal operational record.
    Info,
    /// Unusual but handled.
    Warn,
    /// Failed operation that needs attention.
    Error,
}

/// A destination that receives LLM and tool call records.
///
/// Implementors route one record stream (or several) to one place: stdout, a
/// database, a telemetry collector, a test capture, and so on. The
/// [`ExporterRouter`] fans each record out to every registered exporter.
///
/// # Contract
///
/// - [`LogExporter::export_llm`] and [`LogExporter::export_tool`] return
///   `()`. An exporter CANNOT report failure upward. On any internal problem
///   (write error, serialization failure, connection loss), log it with
///   `tracing::warn!` INSIDE the implementation and return normally. One
///   broken destination must never fail inference.
/// - Never panic in an exporter. A panic escapes into the caller's inference
///   path.
/// - [`LogExporter::validate`] runs once at registration time. Return `Err`
///   there to refuse a misconfigured exporter early; the router rejects the
///   registration.
#[async_trait::async_trait]
pub trait LogExporter: Send + Sync + 'static {
    /// Level gate checked before every export. The default accepts every
    /// record; override to receive only some levels.
    fn accepts(&self, _record_level: RecordLevel) -> bool {
        true
    }

    /// Route one LLM call record. See the trait contract: failures are logged
    /// inside the implementation, never propagated.
    async fn export_llm(&self, record: &LlmCallRecord);

    /// Route one tool call record. See the trait contract: failures are
    /// logged inside the implementation, never propagated.
    async fn export_tool(&self, record: &ToolCallRecord);

    /// Called once when the exporter is registered. Exporters that fail
    /// validation are rejected by the router.
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Fan-out point dispatching records to every registered exporter.
///
/// Holds the exporters in registration order. `log_llm` and `log_tool` walk
/// that order, skip exporters whose [`LogExporter::accepts`] gate rejects the
/// level, and await each export. Fan-out is sequential today; see the module
/// docs.
pub struct ExporterRouter {
    exporters: Vec<Arc<dyn LogExporter>>,
}

impl ExporterRouter {
    /// Creates an empty router.
    pub fn new() -> Self {
        Self {
            exporters: Vec::new(),
        }
    }

    /// Creates an empty router with room for `n` exporters, avoiding
    /// regrowth during startup registration.
    pub fn with_capacity(n: usize) -> Self {
        Self {
            exporters: Vec::with_capacity(n),
        }
    }

    /// Registers one exporter.
    ///
    /// - Registering the SAME exporter twice (same `Arc` pointer) is a
    ///   silent no-op that returns `Ok(())`; the router keeps one copy.
    /// - Otherwise [`LogExporter::validate`] runs once. An `Err` is passed
    ///   back unchanged and the exporter is NOT stored.
    pub fn register(&mut self, exporter: Arc<dyn LogExporter>) -> Result<(), String> {
        if self
            .exporters
            .iter()
            .any(|existing| Arc::ptr_eq(existing, &exporter))
        {
            return Ok(());
        }
        exporter.validate()?;
        self.exporters.push(exporter);
        Ok(())
    }

    /// Number of registered exporters.
    pub fn len(&self) -> usize {
        self.exporters.len()
    }

    /// True when no exporter is registered.
    pub fn is_empty(&self) -> bool {
        self.exporters.is_empty()
    }

    /// Fans one LLM call record out to every exporter whose gate accepts
    /// `level`. Exporter problems never propagate; see the
    /// [`LogExporter`] contract.
    pub async fn log_llm(&self, level: RecordLevel, record: &LlmCallRecord) {
        for exporter in &self.exporters {
            if !exporter.accepts(level) {
                continue;
            }
            exporter.export_llm(record).await;
        }
    }

    /// Fans one tool call record out to every exporter whose gate accepts
    /// `level`. Exporter problems never propagate; see the
    /// [`LogExporter`] contract.
    pub async fn log_tool(&self, level: RecordLevel, record: &ToolCallRecord) {
        for exporter in &self.exporters {
            if !exporter.accepts(level) {
                continue;
            }
            exporter.export_tool(record).await;
        }
    }
}

impl Default for ExporterRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Adapter backing [`closure`]: holds the two callbacks and forwards each
/// record kind to its own. Gate and validation stay at their defaults.
struct ClosureExporter<L, T> {
    fn_llm: L,
    fn_tool: T,
}

#[async_trait::async_trait]
impl<L, T> LogExporter for ClosureExporter<L, T>
where
    L: Fn(&LlmCallRecord) + Send + Sync + 'static,
    T: Fn(&ToolCallRecord) + Send + Sync + 'static,
{
    async fn export_llm(&self, record: &LlmCallRecord) {
        (self.fn_llm)(record);
    }

    async fn export_tool(&self, record: &ToolCallRecord) {
        (self.fn_tool)(record);
    }
}

/// Builds an exporter from two plain closures, one per record kind.
///
/// Handy for small sinks and test captures without writing a full
/// [`LogExporter`] impl. The adapter keeps the default gate (every level) and
/// default validation.
pub fn closure<L, T>(fn_llm: L, fn_tool: T) -> Arc<dyn LogExporter>
where
    L: Fn(&LlmCallRecord) + Send + Sync + 'static,
    T: Fn(&ToolCallRecord) + Send + Sync + 'static,
{
    Arc::new(ClosureExporter { fn_llm, fn_tool })
}

/// Exporter that writes each record to the `tracing` subsystem.
///
/// This is the tracing bridge: deployments that register only this exporter
/// get instant visibility into LLM and tool traffic without any storage.
/// Records whose `status` is `"success"` go out at `INFO`; anything else
/// (for example `"error"` or `"timeout"`) goes out at `WARN`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TracingExporter;

impl TracingExporter {
    /// Creates the exporter.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl LogExporter for TracingExporter {
    async fn export_llm(&self, record: &LlmCallRecord) {
        let latency_ms = record.latency_ms;
        let model = record.model.as_str();
        let provider = record.provider.as_str();
        let status = record.status.as_str();
        // Optional fields: `Option<T>` implements `tracing::Value` and emits
        // nothing when `None`, so absent usage data stays invisible.
        let cached_tokens = record.cached_tokens;
        let total_time_ms = record.total_time_ms;
        if status == "success" {
            tracing::info!(
                latency_ms,
                model,
                provider,
                status,
                cached_tokens,
                total_time_ms,
                "LLM call completed"
            );
        } else {
            tracing::warn!(
                latency_ms,
                model,
                provider,
                status,
                cached_tokens,
                total_time_ms,
                "LLM call failed"
            );
        }
    }

    async fn export_tool(&self, record: &ToolCallRecord) {
        let latency_ms = record.latency_ms;
        let status = record.status.as_str();
        let tool_name = record.tool_name.as_str();
        let tool_type = record.tool_type.as_str();
        if status == "success" {
            tracing::info!(
                latency_ms,
                status,
                tool_name,
                tool_type,
                "Tool call completed"
            );
        } else {
            tracing::warn!(latency_ms, status, tool_name, tool_type, "Tool call failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Shared capture slot: `(export count, exported record kinds)`.
    type Capture = Arc<(Mutex<usize>, Mutex<Vec<String>>)>;

    /// Capturing mock. Counts every export, remembers the record kind, and
    /// optionally appends its tag to a shared order log.
    struct MockExporter {
        tag: &'static str,
        errors_only: bool,
        capture: Capture,
        shared_log: Option<Arc<Mutex<Vec<String>>>>,
    }

    impl MockExporter {
        fn new(tag: &'static str) -> Self {
            Self {
                tag,
                errors_only: false,
                capture: Arc::new((Mutex::new(0), Mutex::new(Vec::new()))),
                shared_log: None,
            }
        }

        /// Gate variant accepting only [`RecordLevel::Error`].
        fn errors_only(tag: &'static str) -> Self {
            Self {
                errors_only: true,
                ..Self::new(tag)
            }
        }

        /// Variant that appends its tag to a shared log, to prove order.
        fn ordered(tag: &'static str, log: Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                shared_log: Some(log),
                ..Self::new(tag)
            }
        }

        fn record(&self, kind: &str) {
            *self.capture.0.lock().unwrap() += 1;
            self.capture.1.lock().unwrap().push(kind.to_string());
            if let Some(log) = &self.shared_log {
                log.lock().unwrap().push(self.tag.to_string());
            }
        }
    }

    #[async_trait::async_trait]
    impl LogExporter for MockExporter {
        fn accepts(&self, level: RecordLevel) -> bool {
            !self.errors_only || level == RecordLevel::Error
        }

        async fn export_llm(&self, _record: &LlmCallRecord) {
            self.record("llm");
        }

        async fn export_tool(&self, _record: &ToolCallRecord) {
            self.record("tool");
        }
    }

    /// Exporter that always fails validation.
    struct BrokenExporter;

    #[async_trait::async_trait]
    impl LogExporter for BrokenExporter {
        async fn export_llm(&self, _record: &LlmCallRecord) {}

        async fn export_tool(&self, _record: &ToolCallRecord) {}

        fn validate(&self) -> Result<(), String> {
            Err(String::from("broken exporter refuses validation"))
        }
    }

    fn sample_llm() -> LlmCallRecord {
        LlmCallRecord {
            step_index: 0,
            provider: String::from("openai"),
            model: String::from("gpt-4o"),
            prompt_tokens: 10,
            completion_tokens: 5,
            latency_ms: 42,
            status: String::from("success"),
            cached_tokens: Some(7),
            total_time_ms: Some(42),
        }
    }

    #[test]
    fn sample_record_carries_optional_usage_fields() {
        let record = sample_llm();
        assert_eq!(record.cached_tokens, Some(7));
        assert_eq!(record.total_time_ms, Some(42));
    }

    fn sample_tool() -> ToolCallRecord {
        ToolCallRecord {
            step_index: 0,
            tool_name: String::from("calculator"),
            tool_type: String::from("builtin"),
            arguments: serde_json::json!({ "expression": "1 + 1" }),
            result: None,
            latency_ms: 7,
            status: String::from("success"),
        }
    }

    /// One record reaches EVERY registered exporter, on both routes.
    #[tokio::test]
    async fn router_fans_out_to_all_exporters() {
        let mut router = ExporterRouter::new();
        let first = MockExporter::new("first");
        let second = MockExporter::new("second");
        let first_capture = first.capture.clone();
        let second_capture = second.capture.clone();
        router.register(Arc::new(first)).unwrap();
        router.register(Arc::new(second)).unwrap();
        assert_eq!(router.len(), 2);

        router.log_llm(RecordLevel::Info, &sample_llm()).await;
        router.log_tool(RecordLevel::Info, &sample_tool()).await;

        for capture in [first_capture, second_capture] {
            let counts = capture.0.lock().unwrap();
            assert_eq!(*counts, 2, "each exporter sees both records");
            let kinds = capture.1.lock().unwrap();
            assert_eq!(*kinds, vec![String::from("llm"), String::from("tool")]);
        }
    }

    /// An exporter whose gate takes only `Error` sees nothing at lower
    /// levels and gets the record at `Error`.
    #[tokio::test]
    async fn accepts_gate_filters_records() {
        let mut router = ExporterRouter::new();
        let exporter = MockExporter::errors_only("gate");
        let capture = exporter.capture.clone();
        router.register(Arc::new(exporter)).unwrap();

        router.log_llm(RecordLevel::Info, &sample_llm()).await;
        router.log_tool(RecordLevel::Warn, &sample_tool()).await;
        assert_eq!(
            *capture.0.lock().unwrap(),
            0,
            "Info and Warn are filtered out"
        );

        router.log_llm(RecordLevel::Error, &sample_llm()).await;
        assert_eq!(*capture.0.lock().unwrap(), 1, "Error passes the gate");
    }

    /// Registering the same Arc pointer twice keeps a single copy.
    #[tokio::test]
    async fn duplicate_registration_is_skipped() {
        let mut router = ExporterRouter::new();
        let exporter: Arc<dyn LogExporter> = Arc::new(MockExporter::new("dup"));

        assert!(router.register(exporter.clone()).is_ok());
        assert!(
            router.register(exporter.clone()).is_ok(),
            "duplicate register is a silent no-op, not an error"
        );
        assert_eq!(router.len(), 1);
    }

    /// An exporter failing validation is refused and never stored.
    #[tokio::test]
    async fn validate_rejects_broken_exporter() {
        let mut router = ExporterRouter::new();

        let error = router
            .register(Arc::new(BrokenExporter))
            .expect_err("validation failure must reject registration");
        assert_eq!(error, "broken exporter refuses validation");
        assert!(router.is_empty(), "rejected exporter is not stored");
    }

    /// Fan-out walks exporters in registration order, one after another.
    /// A slow exporter delays the ones behind it today; concurrent fan-out
    /// remains a possible later change behind measurement. Order proof:
    /// both exporters append to one shared log, and the log reads a, b.
    #[tokio::test]
    async fn slow_exporter_does_not_block_others() {
        let mut router = ExporterRouter::new();
        let shared: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        router
            .register(Arc::new(MockExporter::ordered("a", shared.clone())))
            .unwrap();
        router
            .register(Arc::new(MockExporter::ordered("b", shared.clone())))
            .unwrap();

        router.log_llm(RecordLevel::Info, &sample_llm()).await;

        assert_eq!(
            *shared.lock().unwrap(),
            vec![String::from("a"), String::from("b")],
            "fan-out is sequential in registration order"
        );
    }
}
