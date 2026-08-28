//! Cordis logger — buffered, exporter-pluggable, fiber-aware logging.
//!
//! Mirrors the reference-kernel timer/logger primitive family (upstream-style
//! semantics, native Rust shape): a [`LoggerService`] keeps a bounded ring of
//! recent [`Message`]s, fans accepted messages out to registered
//! [`Exporter`] sinks, and gates work behind a cheap [`LoggerService::enabled`]
//! check so disabled paths never assemble arguments.
//!
//! Semantics:
//! - **Kinds** ([`LogKind`]): `Error` / `Warn` / `Info` / `Debug`. Each kind
//!   maps onto a [`LogLevel`] severity where lower = more severe; a message is
//!   emitted when its kind severity is at or above (more severe than or equal
//!   to) the effective threshold.
//! - **Levels per name**: [`LoggerService::set_level`] pins a threshold for one
//!   logger name; unlisted names fall back to [`LoggerService::set_default_level`]
//!   (default: `Debug`, i.e. pass-everything).
//! - **Per-fiber override**: rides the existing intercept channel — install
//!   [`LoggerIntercept`] via `ctx.intercept(..)` and every write through that
//!   context handle resolves it at write time. `name: None` matches every
//!   logger; `level: Some(..)` replaces the effective threshold for matching
//!   writes. Nothing on the hot structs changes; resolution uses the public
//!   relaxed read.
//! - **Exporters**: [`ExporterConfig`] carries a per-name level map and a
//!   `max_length` text cap; unlisted names pass unrestricted. Registration
//!   returns a [`Disposable`] whose disposal removes the sink (effect-owned).
//! - **Rendering**: [`Message::render`] applies printf-style placeholders
//!   (`%s %d %i %f %o %O %c %C %%`) when the leading argument is a format
//!   string, otherwise joins arguments with spaces. `%c` colorizes with a
//!   stable per-name hash over the ANSI16 palette; `%C` adds bold.
//! - **Derived names**: [`hyphenate`] turns `CamelCase` into `kebab-case`;
//!   [`derived_name`] applies it to a type's short name for logger naming.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::context::Context;
use crate::effect::Disposable;
use crate::service::Service;

// ---------------------------------------------------------------------------
// Kinds and levels
// ---------------------------------------------------------------------------

/// Severity family of one log record. Maps onto [`LogLevel`] severities where
/// `Error` is the most severe and `Debug` the least.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogKind {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogKind {
    /// Severity rank: lower = more severe. Used by threshold comparisons.
    pub fn severity(self) -> u8 {
        match self {
            LogKind::Error => 0,
            LogKind::Warn => 1,
            LogKind::Info => 2,
            LogKind::Debug => 3,
        }
    }

    /// Stable lowercase label (`"error"`, `"warn"`, `"info"`, `"debug"`).
    pub fn as_str(self) -> &'static str {
        match self {
            LogKind::Error => "error",
            LogKind::Warn => "warn",
            LogKind::Info => "info",
            LogKind::Debug => "debug",
        }
    }
}

/// Numeric severity threshold. A kind passes when
/// `kind.severity() <= level.0` (more-severe-or-equal). Ordering follows the
/// ranks, so `LogLevel::INFO < LogLevel::DEBUG` and `INFO.allows(DEBUG)` is
/// false while `INFO.allows(WARN)` is true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogLevel(u8);

impl LogLevel {
    pub const ERROR: LogLevel = LogLevel(0);
    pub const WARN: LogLevel = LogLevel(1);
    pub const INFO: LogLevel = LogLevel(2);
    pub const DEBUG: LogLevel = LogLevel(3);

    /// `true` when a message of `kind` meets this threshold.
    pub fn allows(self, kind: LogKind) -> bool {
        kind.severity() <= self.0
    }
}

impl From<LogKind> for LogLevel {
    fn from(kind: LogKind) -> Self {
        LogLevel(kind.severity())
    }
}

// ---------------------------------------------------------------------------
// Arguments and messages
// ---------------------------------------------------------------------------

/// One printf argument. `From` impls keep call sites terse:
/// `vec!["user".into(), 42.into()]`.
#[derive(Debug, Clone, PartialEq)]
pub enum LogArg {
    String(String),
    Integer(i64),
    Unsigned(u64),
    Float(f64),
    Bool(bool),
    /// Structured payload; `%o` renders compact JSON, `%O` pretty JSON.
    Object(serde_json::Value),
}

impl From<&str> for LogArg {
    fn from(v: &str) -> Self {
        LogArg::String(v.to_string())
    }
}

impl From<String> for LogArg {
    fn from(v: String) -> Self {
        LogArg::String(v)
    }
}

impl From<i64> for LogArg {
    fn from(v: i64) -> Self {
        LogArg::Integer(v)
    }
}

impl From<i32> for LogArg {
    fn from(v: i32) -> Self {
        LogArg::Integer(v as i64)
    }
}

impl From<u64> for LogArg {
    fn from(v: u64) -> Self {
        LogArg::Unsigned(v)
    }
}

impl From<usize> for LogArg {
    fn from(v: usize) -> Self {
        LogArg::Unsigned(v as u64)
    }
}

impl From<f64> for LogArg {
    fn from(v: f64) -> Self {
        LogArg::Float(v)
    }
}

impl From<bool> for LogArg {
    fn from(v: bool) -> Self {
        LogArg::Bool(v)
    }
}

impl From<serde_json::Value> for LogArg {
    fn from(v: serde_json::Value) -> Self {
        LogArg::Object(v)
    }
}

/// ANSI16 foreground palette: `30..=37` plus bright `90..=97`.
const ANSI16: [u8; 16] = [30, 31, 32, 33, 34, 35, 36, 37, 90, 91, 92, 93, 94, 95, 96, 97];

/// FNV-1a 64-bit — stable across processes and platforms, so a given logger
/// name always lands on the same palette slot.
fn name_color_code(name: &str) -> u8 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    ANSI16[(hash % ANSI16.len() as u64) as usize]
}

fn render_object(value: &serde_json::Value, pretty: bool) -> String {
    if !pretty {
        return value.to_string();
    }
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Plain (uncolored) text of one argument. Objects render as compact JSON
/// regardless of the specifier that reached them; scalars render naturally.
fn arg_text(arg: &LogArg, pretty_objects: bool) -> String {
    match arg {
        LogArg::String(s) => s.clone(),
        LogArg::Integer(v) => v.to_string(),
        LogArg::Unsigned(v) => v.to_string(),
        LogArg::Float(v) => v.to_string(),
        LogArg::Bool(v) => v.to_string(),
        LogArg::Object(v) => render_object(v, pretty_objects),
    }
}

/// One buffered log record.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Monotonic per-service sequence starting at 1.
    pub sequence: u64,
    /// Milliseconds since the Unix epoch at write time.
    pub timestamp_ms: u64,
    /// Logger name (usually the hyphenated component name).
    pub name: String,
    pub kind: LogKind,
    /// Severity of `kind` at write time (kept numeric for exporters).
    pub level: LogLevel,
    pub args: Vec<LogArg>,
    /// Identity of the emitting context's registration fiber. Kernel fibers
    /// carry no user-facing name, so this is a stable per-fiber label derived
    /// without touching any shared struct.
    pub fiber_name: String,
}

impl Message {
    /// Render the arguments to display text.
    ///
    /// When the leading argument is a `String` containing `%`, it is treated
    /// as a printf-style format consuming subsequent arguments (`%s` string,
    /// `%d`/`%i` integer, `%f` float, `%o` compact object, `%O` pretty object,
    /// `%c` colorized with the per-name palette slot, `%C` bold colorized,
    /// `%%` literal percent). Unknown specifiers and exhausted arguments stay
    /// literal; unconsumed arguments are appended space-separated. Without a
    /// format head, arguments are joined with single spaces.
    pub fn render(&self) -> String {
        let fmt_head = match self.args.first() {
            Some(LogArg::String(s)) if s.contains('%') => Some(s.as_str()),
            _ => None,
        };

        let mut out = String::new();
        let Some(fmt) = fmt_head else {
            for (i, arg) in self.args.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(&arg_text(arg, false));
            }
            return out;
        };

        let mut next = 1usize;
        let chars: Vec<char> = fmt.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            let ch = chars[i];
            if ch != '%' {
                out.push(ch);
                i += 1;
                continue;
            }
            i += 1;
            let Some(&spec) = chars.get(i) else {
                out.push('%');
                break;
            };
            i += 1;
            match spec {
                '%' => out.push('%'),
                's' | 'd' | 'i' | 'f' | 'o' | 'O' => match self.args.get(next) {
                    Some(arg) => {
                        out.push_str(&arg_text(arg, spec == 'O'));
                        next += 1;
                    }
                    None => {
                        out.push('%');
                        out.push(spec);
                    }
                },
                'c' | 'C' => match self.args.get(next) {
                    Some(arg) => {
                        let text = arg_text(arg, false);
                        let code = name_color_code(&self.name);
                        let _ = write!(out, "\x1b[{code}m{}\x1b[0m", text);
                        if spec == 'C' {
                            // Bold variant: rewrite the intro sequence.
                            let body = &out[out.len() - text.len() - 5..out.len()];
                            let _ = body;
                            let colored =
                                format!("\x1b[{code};1m{text}\x1b[0m");
                            out.truncate(out.len() - text.len() - 5 - ("\x1b[0m".len()));
                            let _ = colored;
                            let _ = write!(out, "\x1b[{code};1m{text}\x1b[0m");
                        }
                        next += 1;
                    }
                    None => {
                        out.push('%');
                        out.push(spec);
                    }
                },
                other => {
                    out.push('%');
                    out.push(other);
                }
            }
        }

        // Unconsumed trailing arguments are appended space-separated.
        for arg in self.args.iter().skip(next) {
            out.push(' ');
            out.push_str(&arg_text(arg, false));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Exporters
// ---------------------------------------------------------------------------

/// Per-sink configuration: per-name thresholds (unlisted names pass freely)
/// and a maximum rendered-text length handed to the sink.
#[derive(Debug, Clone)]
pub struct ExporterConfig {
    /// Logger name → minimum severity the sink accepts.
    pub levels: HashMap<String, LogLevel>,
    /// Character cap applied to the rendered text per message.
    pub max_length: usize,
}

impl Default for ExporterConfig {
    fn default() -> Self {
        Self {
            levels: HashMap::new(),
            max_length: 4096,
        }
    }
}

impl ExporterConfig {
    pub fn new(levels: HashMap<String, LogLevel>, max_length: usize) -> Self {
        Self { levels, max_length }
    }

    /// Threshold for one logger name; unlisted names pass unrestricted.
    pub fn threshold(&self, name: &str) -> LogLevel {
        self.levels
            .get(name)
            .copied()
            .unwrap_or(LogLevel::DEBUG)
    }

    /// Char-boundary-safe truncation to `max_length` characters.
    pub fn truncate(&self, text: String) -> String {
        if self.max_length == 0 {
            return String::new();
        }
        if text.chars().count() <= self.max_length {
            return text;
        }
        text.chars().take(self.max_length).collect()
    }
}

/// One log sink. Receives the structured [`Message`] plus the rendered text
/// (already truncated to the sink's `max_length`). Exporters must not panic;
/// they run inline on the writer's thread.
pub trait Exporter: Send + Sync + 'static {
    fn export(&self, message: &Message, text: &str);
}

struct ExportSlot {
    exporter: Arc<dyn Exporter>,
    config: ExporterConfig,
    /// `Arc::as_ptr` address — the removal key carried by the disposable.
    key: usize,
}

// ---------------------------------------------------------------------------
// Per-fiber override via the intercept channel
// ---------------------------------------------------------------------------

/// Per-fiber logger override riding the existing intercept channel. Install
/// with `ctx.intercept(LoggerIntercept { .. })`; every write through that
/// context handle resolves it at write time.
///
/// - `name: None` matches every logger; `Some(n)` matches logger `n` only.
/// - `level: Some(l)` replaces the effective threshold for matching writes
///   (overriding both the per-name and default configuration).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoggerIntercept {
    pub name: Option<String>,
    pub level: Option<LogLevel>,
}

impl Service for LoggerIntercept {
    fn name(&self) -> &'static str {
        "logger_intercept"
    }
}

impl LoggerIntercept {
    pub fn matches(&self, logger_name: &str) -> bool {
        match &self.name {
            Some(n) => n == logger_name,
            None => true,
        }
    }
}

// ---------------------------------------------------------------------------
// LoggerService
// ---------------------------------------------------------------------------

/// Bounded buffer of recent messages plus exporter fan-out.
///
/// Provide it once on the root context (`ctx.provide(LoggerService::new())`);
/// writes go through the [`Context`] facade methods defined in this module or
/// directly on the service. Every write path:
/// 1. resolves the effective threshold (per-name → intercept → default),
/// 2. bails BEFORE argument assembly when the kind fails the gate,
/// 3. appends the message to the ring (dropping the oldest at capacity),
/// 4. fans out to every exporter accepting `(name, kind)`.
pub struct LoggerService {
    buffer: RwLock<VecDeque<Arc<Message>>>,
    capacity: usize,
    seq: AtomicU64,
    levels: RwLock<HashMap<String, LogLevel>>,
    default_level: RwLock<LogLevel>,
    exporters: RwLock<Vec<ExportSlot>>,
}

impl Service for LoggerService {
    fn name(&self) -> &'static str {
        "logger_service"
    }
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Stable per-fiber label derived from the emitting context's registration
/// fiber pointer — no shared struct carries fiber names, and this stays unique
/// per context without any lifecycle coupling.
fn fiber_label(ctx: &Context) -> String {
    fiber_label_from_fiber(&ctx.fiber())
}

fn fiber_label_from_fiber(fiber: &Arc<crate::fiber::Fiber>) -> String {
    let ptr = Arc::as_ptr(fiber) as *const () as usize;
    format!("fiber-{ptr:x}")
}

impl Default for LoggerService {
    fn default() -> Self {
        Self::with_capacity(1000)
    }
}

impl LoggerService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: RwLock::new(VecDeque::with_capacity(capacity.min(1024))),
            capacity: capacity.max(1),
            seq: AtomicU64::new(0),
            levels: RwLock::new(HashMap::new()),
            default_level: RwLock::new(LogLevel::DEBUG),
            exporters: RwLock::new(Vec::new()),
        }
    }

    /// Pin the threshold for one logger name.
    pub fn set_level(&self, name: impl Into<String>, level: LogLevel) {
        self.levels.write().insert(name.into(), level);
    }

    /// Clear a pinned per-name threshold.
    pub fn clear_level(&self, name: &str) {
        self.levels.write().remove(name);
    }

    /// Threshold for names without a pinned entry. Default: `Debug` (everything).
    pub fn set_default_level(&self, level: LogLevel) {
        *self.default_level.write() = level;
    }

    /// Effective threshold for `name` on `ctx`: per-name pin, else the
    /// intercept override when one matches, else the service default.
    fn effective_threshold(&self, ctx: &Context, name: &str) -> LogLevel {
        let mut level = *self.default_level.read();
        if let Some(pinned) = self.levels.read().get(name) {
            level = *pinned;
        }
        if let Some(intercept) = ctx.get_relaxed::<LoggerIntercept>() {
            if intercept.matches(name) {
                if let Some(forced) = intercept.level {
                    level = forced;
                }
            }
        }
        level
    }

    /// Cheap gate: would a `kind` write from `name` on `ctx` be recorded?
    /// Callers assembling expensive arguments SHOULD consult this first (or
    /// use [`LoggerService::log_with`], which applies it automatically).
    pub fn enabled(&self, ctx: &Context, name: &str, kind: LogKind) -> bool {
        self.effective_threshold(ctx, name).allows(kind)
    }

    /// Core write path. `assemble` runs ONLY when the gate accepts — disabled
    /// paths never pay for argument construction.
    pub fn log_with<F>(&self, ctx: &Arc<Context>, name: &str, kind: LogKind, assemble: F)
    where
        F: FnOnce() -> Vec<LogArg>,
    {
        if !self.enabled(ctx, name, kind) {
            return;
        }
        self.emit(fiber_label(ctx), name, kind, assemble());
    }

    /// Eager variant: arguments are already built. Still gated.
    pub fn log(&self, ctx: &Arc<Context>, name: &str, kind: LogKind, args: Vec<LogArg>) {
        self.log_with(ctx, name, kind, || args)
    }

    /// Write path for callers holding only `&Context`: the owning fiber is
    /// passed explicitly (via [`Context::fiber`]) instead of recovered from
    /// an `Arc<Context>` handle. Same gating as [`Self::log_with`].
    pub fn log_ref<F>(
        &self,
        fiber: &Arc<crate::fiber::Fiber>,
        scope: &Context,
        name: &str,
        kind: LogKind,
        assemble: F,
    ) where
        F: FnOnce() -> Vec<LogArg>,
    {
        if !self.effective_threshold(scope, name).allows(kind) {
            return;
        }
        self.emit(fiber_label_from_fiber(fiber), name, kind, assemble());
    }

    pub fn error(&self, ctx: &Arc<Context>, name: &str, args: Vec<LogArg>) {
        self.log(ctx, name, LogKind::Error, args)
    }

    pub fn warn(&self, ctx: &Arc<Context>, name: &str, args: Vec<LogArg>) {
        self.log(ctx, name, LogKind::Warn, args)
    }

    pub fn info(&self, ctx: &Arc<Context>, name: &str, args: Vec<LogArg>) {
        self.log(ctx, name, LogKind::Info, args)
    }

    pub fn debug(&self, ctx: &Arc<Context>, name: &str, args: Vec<LogArg>) {
        self.log(ctx, name, LogKind::Debug, args)
    }

    fn emit(&self, fiber_name: String, name: &str, kind: LogKind, args: Vec<LogArg>) {
        let sequence = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let message = Arc::new(Message {
            sequence,
            timestamp_ms: unix_now_ms(),
            name: name.to_string(),
            kind,
            level: LogLevel::from(kind),
            args,
            fiber_name,
        });

        {
            let mut buffer = self.buffer.write();
            buffer.push_back(message.clone());
            while buffer.len() > self.capacity {
                buffer.pop_front();
            }
        }

        // Snapshot the slots under a short lock; sinks run unlocked so an
        // exporter that logs back cannot deadlock the registry.
        let slots: Vec<(Arc<dyn Exporter>, ExporterConfig)> = self
            .exporters
            .read()
            .iter()
            .filter(|slot| slot.config.threshold(name).allows(kind))
            .map(|slot| (slot.exporter.clone(), slot.config.clone()))
            .collect();
        if slots.is_empty() {
            return;
        }
        let rendered = message.render();
        for (exporter, config) in slots {
            let text = config.truncate(rendered.clone());
            exporter.export(&message, &text);
        }
    }

    /// Register a sink; the returned [`Disposable`] removes it on disposal.
    pub fn register(
        self: &Arc<Self>,
        exporter: Arc<dyn Exporter>,
        config: ExporterConfig,
    ) -> Box<dyn Disposable> {
        let key = Arc::as_ptr(&exporter) as *const () as usize;
        self.exporters.write().push(ExportSlot {
            exporter,
            config,
            key,
        });
        let weak = Arc::downgrade(self);
        Box::new(move || {
            if let Some(service) = weak.upgrade() {
                service.exporters.write().retain(|slot| slot.key != key);
            }
        })
    }

    /// Snapshot of the buffered messages, oldest first. Clones `Arc`s only.
    pub fn snapshot(&self) -> Vec<Arc<Message>> {
        self.buffer.read().iter().cloned().collect()
    }

    pub fn buffered_len(&self) -> usize {
        self.buffer.read().len()
    }
}

// ---------------------------------------------------------------------------
// Derived names
// ---------------------------------------------------------------------------

/// `CamelCase` → `kebab-case`. Handles acronym heads (`HTTPServer` →
/// `http-server`), preserves digits, leaves kebab input untouched.
pub fn hyphenate(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len() + 4);
    for (i, &ch) in chars.iter().enumerate() {
        let prev = if i > 0 { chars[i - 1] } else { '\0' };
        let next = chars.get(i + 1).copied();
        let boundary = ch.is_uppercase()
            && i > 0
            && prev != '-'
            && (!prev.is_uppercase() || matches!(next, Some(n) if n.is_ascii_lowercase()));
        if boundary {
            out.push('-');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Hyphenated short name of a type: the last `::` segment of
/// `std::any::type_name`, kebab-cased. Intended for logger naming:
/// `ctx.info(&derived_name::<Self>(), ..)`.
pub fn derived_name<T: ?Sized>() -> String {
    let full = std::any::type_name::<T>();
    let short = full.rsplit("::").next().unwrap_or(full);
    hyphenate(short)
}

// ---------------------------------------------------------------------------
// Context facade — additive only; no hot-struct changes
// ---------------------------------------------------------------------------

impl Context {
    /// Gated write with lazy argument assembly through the provided
    /// [`LoggerService`] (no-op when absent). The intercept channel is
    /// consulted on `self`, so per-fiber overrides apply to child contexts.
    pub fn log_with<F>(&self, name: &str, kind: LogKind, assemble: F)
    where
        F: FnOnce() -> Vec<LogArg>,
    {
        if let Some(logger) = self.get::<LoggerService>() {
            logger.log_ref(&self.fiber(), self, name, kind, assemble);
        }
    }

    /// Gated write with pre-built arguments.
    pub fn log(&self, name: &str, kind: LogKind, args: Vec<LogArg>) {
        self.log_with(name, kind, || args)
    }

    pub fn info(&self, name: &str, args: Vec<LogArg>) {
        self.log(name, LogKind::Info, args)
    }

    pub fn warn(&self, name: &str, args: Vec<LogArg>) {
        self.log(name, LogKind::Warn, args)
    }

    pub fn debug(&self, name: &str, args: Vec<LogArg>) {
        self.log(name, LogKind::Debug, args)
    }

    pub fn error(&self, name: &str, args: Vec<LogArg>) {
        self.log(name, LogKind::Error, args)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    struct CountingExporter {
        seen: Mutex<Vec<String>>,
    }

    impl CountingExporter {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
            })
        }

        fn texts(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Exporter for CountingExporter {
        fn export(&self, _message: &Message, text: &str) {
            self.seen.lock().unwrap().push(text.to_string());
        }
    }

    fn service() -> Arc<LoggerService> {
        Arc::new(LoggerService::new())
    }

    #[test]
    fn buffer_bounded_at_capacity_snapshot_reads() {
        let logger = LoggerService::with_capacity(4);
        let ctx = Context::new_root();
        for i in 0..10u64 {
            logger.log(&ctx, "ring", LogKind::Info, vec![i.into()]);
        }

        assert_eq!(logger.buffered_len(), 4);
        let snap = logger.snapshot();
        assert_eq!(snap.len(), 4);
        // Oldest evicted: retention window is the LAST four sequences.
        let seqs: Vec<u64> = snap.iter().map(|m| m.sequence).collect();
        assert_eq!(seqs, vec![7, 8, 9, 10]);
        // Snapshot reads are detached views: further writes leave it alone.
        logger.log(&ctx, "ring", LogKind::Info, vec![99u64.into()]);
        assert_eq!(snap.len(), 4);
        assert_eq!(logger.buffered_len(), 4);
        assert_eq!(logger.snapshot()[3].args[0], LogArg::Unsigned(99));
    }

    #[tokio::test]
    async fn exporter_disposal_removes_sink() {
        let logger = service();
        let ctx = Context::new_root();
        let sink = CountingExporter::new();
        let handle = logger.register(
            sink.clone(),
            ExporterConfig {
                max_length: 64,
                ..ExporterConfig::default()
            },
        );

        logger.info(&ctx, "sink-test", vec!["one".into()]);
        assert_eq!(sink.texts(), vec!["one".to_string()]);

        handle.dispose();
        logger.info(&ctx, "sink-test", vec!["two".into()]);
        // Disposed sinks stop receiving; buffer still records.
        assert_eq!(sink.texts(), vec!["one".to_string()]);
        assert_eq!(logger.buffered_len(), 2);
    }

    #[test]
    fn level_routing_per_name_with_default_fallback() {
        let logger = service();
        let ctx = Context::new_root();

        // Default fallback: Warn admits warn/error, drops info/debug.
        logger.set_default_level(LogLevel::WARN);
        assert!(logger.enabled(&ctx, "quiet", LogKind::Warn));
        assert!(!logger.enabled(&ctx, "quiet", LogKind::Info));

        // Per-name pin overrides the default for that name only.
        logger.set_level("loud", LogLevel::DEBUG);
        assert!(logger.enabled(&ctx, "loud", LogKind::Debug));
        assert!(!logger.enabled(&ctx, "still-quiet", LogKind::Debug));

        // Routing is observable in the buffer.
        logger.debug(&ctx, "loud", vec!["kept".into()]);
        logger.debug(&ctx, "still-quiet", vec!["dropped".into()]);
        let names: Vec<String> = logger
            .snapshot()
            .iter()
            .map(|m| m.name.clone())
            .collect();
        assert_eq!(names, vec!["loud".to_string()]);

        // Raising the default flips previously-dropped names.
        logger.set_default_level(LogLevel::DEBUG);
        logger.debug(&ctx, "still-quiet", vec!["kept-now".into()]);
        assert_eq!(logger.snapshot().len(), 2);

        // Clearing the pin falls back again.
        logger.clear_level("loud");
        assert_eq!(
            logger.effective_threshold(&ctx, "loud"),
            LogLevel::DEBUG
        );
    }

    #[test]
    fn printf_placeholders_format_correctly() {
        let logger = service();
        let ctx = Context::new_root();

        logger.info(
            &ctx,
            "fmt",
            vec![
                "%s=%d %i %f %o %% %q trailing".into(),
                "user".into(),
                42i64.into(),
                (-7i64).into(),
                2.5f64.into(),
                serde_json::json!({"a": 1}).into(),
                "extra".into(),
            ],
        );
        let text = logger.snapshot()[0].render();
        let pretty_b = serde_json::to_string_pretty(&serde_json::json!({"b": [1]})).unwrap();
        assert_eq!(
            text,
            "user=42 -7 2.5 {\"a\":1} % %q trailing extra".to_string()
        );

        // %O renders pretty JSON; %% collapses; %c colorizes with the
        // name-stable palette slot.
        logger.debug(
            &ctx,
            "fmt",
            vec![
                "%O|%c|done".into(),
                serde_json::json!({"b": [1]}).into(),
                "tag".into(),
            ],
        );
        let colored = logger.snapshot()[1].render();
        assert!(colored.starts_with(&pretty_b[..pretty_b.len() - 1]));
        assert!(colored.contains("\x1b["));
        assert!(colored.ends_with("|done"));

        // Exhausted arguments leave the specifier literal.
        let bare = Message {
            sequence: 0,
            timestamp_ms: 0,
            name: "fmt".into(),
            kind: LogKind::Info,
            level: LogLevel::INFO,
            args: vec!["%d items".into()],
            fiber_name: String::new(),
        };
        assert_eq!(bare.render(), "%d items");

        // Without a format head, arguments join with spaces.
        let plain = Message {
            sequence: 0,
            timestamp_ms: 0,
            name: "fmt".into(),
            kind: LogKind::Info,
            level: LogLevel::INFO,
            args: vec!["a".into(), 1i64.into(), true.into()],
            fiber_name: String::new(),
        };
        assert_eq!(plain.render(), "a 1 true");
    }

    #[test]
    fn enabled_gate_skips_arg_assembly() {
        let logger = service();
        let ctx = Context::new_root();
        logger.set_default_level(LogLevel::WARN);

        let assembled = AtomicBool::new(false);
        let flip = |flag: &AtomicBool| flag.store(true, Ordering::SeqCst);

        // Disabled path: the assembler never runs.
        logger.log_with(&ctx, "gate", LogKind::Debug, || {
            flip(&assembled);
            vec!["expensive".into()]
        });
        assert!(!assembled.load(Ordering::SeqCst));
        assert!(logger.snapshot().is_empty());

        // Enabled path: assembler runs exactly once and the record lands.
        logger.warn(&ctx, "gate", vec![]);
        logger.log_with(&ctx, "gate", LogKind::Warn, || {
            flip(&assembled);
            vec!["cheap-enough".into()]
        });
        assert!(assembled.load(Ordering::SeqCst));
        assert_eq!(logger.buffered_len(), 2);

        // The public gate agrees with the routing decision.
        assert!(logger.enabled(&ctx, "gate", LogKind::Warn));
        assert!(!logger.enabled(&ctx, "gate", LogKind::Info));
    }

    #[tokio::test]
    async fn logger_intercept_overrides_level() {
        let root = Context::new_root();
        root.provide(LoggerService::new());

        // Baseline on the root handle: default Debug passes everything.
        let logger = root.get::<LoggerService>().unwrap();
        root.debug("svc", vec!["root-passes".into()]);
        assert_eq!(logger.buffered_len(), 1);

        // Fiber-scoped override: only "svc" drops to Error-only on the child.
        let child = root.intercept(LoggerIntercept {
            name: Some("svc".into()),
            level: Some(LogLevel::ERROR),
        });
        child.debug("svc", vec!["suppressed".into()]);
        child.error("svc", vec!["survives".into()]);
        // Non-matching names keep the ambient configuration.
        child.debug("other", vec!["other-passes".into()]);

        let msgs = logger.snapshot();
        let texts: Vec<&str> = msgs
            .iter()
            .map(|m| arg_text(&m.args[0], false).leak() as &str)
            .collect();
        assert_eq!(texts, ["root-passes", "survives", "other-passes"]);

        // Wildcard intercept: name=None forces the level for every logger.
        let wild = root.intercept(LoggerIntercept {
            name: None,
            level: Some(LogLevel::ERROR),
        });
        wild.info("anything", vec!["blocked".into()]);
        assert_eq!(logger.buffered_len(), 3);
    }

    #[test]
    fn hyphenate_and_derived_names() {
        assert_eq!(hyphenate("ToolsService"), "tools-service");
        assert_eq!(hyphenate("HTTPServer"), "http-server");
        assert_eq!(hyphenate("already-kebab"), "already-kebab");
        assert_eq!(hyphenate("V2Plan"), "v2-plan");
        assert_eq!(derived_name::<LoggerService>(), "logger-service");
    }

    #[test]
    fn exporter_config_truncates_and_filters() {
        let mut levels = HashMap::new();
        levels.insert("db".to_string(), LogLevel::WARN);
        let config = ExporterConfig::new(levels, 5);
        assert_eq!(config.threshold("db"), LogLevel::WARN);
        assert_eq!(config.threshold("other"), LogLevel::DEBUG);
        assert_eq!(config.truncate("abcdefg".into()), "abcde");
        assert_eq!(config.truncate("ok".into()), "ok");
        assert_eq!(ExporterConfig::default().truncate("héllo".into()), "héllo");

        let wide = ExporterConfig {
            max_length: 0,
            ..ExporterConfig::default()
        };
        assert_eq!(wide.truncate("anything".into()), "");
    }

    #[test]
    fn colorization_is_stable_per_name() {
        let mk = |name: &str| {
            Message {
                sequence: 0,
                timestamp_ms: 0,
                name: name.into(),
                kind: LogKind::Info,
                level: LogLevel::INFO,
                args: vec!["%c".into(), "payload".into()],
                fiber_name: String::new(),
            }
            .render()
        };
        let a1 = mk("alpha");
        let a2 = mk("alpha");
        assert_eq!(a1, a2);
        assert!(a1.contains("\x1b["));
        assert!(a1.contains("payload"));
        // Bold variant (%C) differs from plain (%c).
        let bold = Message {
            sequence: 0,
            timestamp_ms: 0,
            name: "alpha".into(),
            kind: LogKind::Info,
            level: LogLevel::INFO,
            args: vec!["%C".into(), "payload".into()],
            fiber_name: String::new(),
        }
        .render();
        assert_ne!(bold, a1);
    }
}
