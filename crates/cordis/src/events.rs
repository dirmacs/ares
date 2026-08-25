use parking_lot::{Mutex, RwLock};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::effect::Disposable;
use crate::service::{CordisError, Service};
use crate::EventId;

/// Collected failures from one parallel event dispatch.
///
/// `Dispatch::Parallel` fans out to every registered listener and joins all
/// tasks; instead of surfacing only the first error, every failure is
/// recorded here as a `(listener name, message)` pair. Rendering goes through
/// [`summarize_listener_errors`], and the dispatch result carries the summary
/// text wrapped in [`CordisError::Internal`].
#[derive(Debug)]
pub struct AggregateError {
    pub errors: Vec<(String, String)>,
}

impl std::fmt::Display for AggregateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format_listener_errors(&self.errors))
    }
}

impl std::error::Error for AggregateError {}

/// Pure formatter shared by [`AggregateError`] and the parallel-dispatch warn
/// line: `"N listener failures: name: message; name: message"` (singular
/// wording for exactly one entry).
pub fn summarize_listener_errors(errors: Vec<(String, String)>) -> String {
    format_listener_errors(&errors)
}

fn format_listener_errors(errors: &[(String, String)]) -> String {
    if errors.is_empty() {
        return "0 listener failures".to_string();
    }
    let joined = errors
        .iter()
        .map(|(name, message)| format!("{name}: {message}"))
        .collect::<Vec<_>>()
        .join("; ");
    if errors.len() == 1 {
        format!("1 listener failure: {joined}")
    } else {
        format!("{} listener failures: {joined}", errors.len())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Emit,
    Parallel,
    Serial,
    Bail,
    Waterfall,
}

/// Registration options for a flat listener, mirroring the reference-kernel
/// `EventOptions` shape.
///
/// * `prepend: true` inserts the listener at the FRONT of the dispatch-order
///   list (upstream `unshift`), so it runs before previously registered
///   listeners of the same event.
/// * `global: true` marks the listener as realm-agnostic: context filters
///   ([`EventsService::emit_filtered`]) never exclude it.
///
/// The historical [`EventsService::on`] / [`EventsService::once`] paths
/// delegate with `EventOptions::default()` (both `false`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventOptions {
    pub prepend: bool,
    pub global: bool,
}

/// Per-listener participation predicate for [`EventsService::emit_filtered`].
///
/// Receives the listener's registration [`EventOptions`] — the only
/// per-listener metadata this kernel records — and decides whether that
/// non-global listener participates in the filtered dispatch. Global
/// listeners bypass the filter entirely and never invoke it.
pub type ListenerFilter = Box<dyn Fn(&EventOptions) -> bool + Send + Sync>;

/// Synthetic event names used by this crate's own unit tests to exercise
/// dispatch mechanics (ordering, bail, disposal). They are not product
/// contracts and bypass catalog validation when built for tests.
const MECHANICS_TEST_EVENTS: &[&str] = &[
    "test",
    "test.event",
    "gone",
    "gone.wf",
    "parallel.result",
    "serial.bail",
    "serial.identity",
    "serial.test",
    "bail.test",
    "emit.test",
    "emit.counter",
    "wf.next",
    "wf.short",
    "wf.empty",
    "par.test",
    "par2.test",
    "par.agg",
    "par.solo",
    "around.empty",
    "around.wrap",
    "around.short",
    "once.test",
    "once.bail",
    // C1 EventOptions mechanics tests.
    "prepend.test",
    "filtered.test",
    "global.test",
];

fn bypasses_catalog(event: &str) -> bool {
    cfg!(test) && MECHANICS_TEST_EVENTS.contains(&event)
}

/// Debug-only contract enforcement. Compiles out in release builds.
fn debug_enforce_dispatch(event: &EventId, mode: Dispatch) {
    if bypasses_catalog(event) {
        return;
    }
    if let Err(msg) = crate::events_catalog::validate_dispatch(event, mode) {
        debug_assert!(false, "{msg}");
    }
}

/// Debug-only listener-registry enforcement. Compiles out in release builds.
fn debug_enforce_listener(event: &EventId, waterfall_registration: bool) {
    if bypasses_catalog(event) {
        return;
    }
    if let Err(msg) = crate::events_catalog::validate_listener(event, waterfall_registration) {
        debug_assert!(false, "{msg}");
    }
}

type Handler = Arc<
    dyn Fn(
            serde_json::Value,
        ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>>
        + Send
        + Sync,
>;

/// The `next` continuation handed to a waterfall handler.  It advances to the
/// next registered waterfall handler, or returns the passed payload unchanged once
/// the chain is exhausted.  It is `FnOnce`: a handler may call `next` at most once,
/// mirroring Cordis `next()` semantics.
pub type WaterfallNext = Box<
    dyn FnOnce(
            serde_json::Value,
        )
            -> Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>>
        + Send,
>;

/// A Cordis `waterfall` around-middleware handler.  It receives the current payload
/// plus a `next` continuation.  Calling `next(payload)` runs the downstream chain and
/// yields its result for further transformation; choosing NOT to call `next`
/// short-circuits the chain (any later handlers do not run).
type WaterfallHandler = Arc<
    dyn Fn(
            serde_json::Value,
            WaterfallNext,
        ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
struct HandlerSlot {
    cancelled: Arc<AtomicBool>,
    /// Full registration [`EventOptions`]. `global` exempts the listener from
    /// filtered-dispatch exclusion outright; the whole option set is what
    /// [`EventsService::emit_filtered`]'s filter gets to inspect.
    options: EventOptions,
    handler: Handler,
}

#[derive(Clone)]
struct WaterfallSlot {
    cancelled: Arc<AtomicBool>,
    handler: WaterfallHandler,
}

pub struct EventsService {
    handlers: RwLock<HashMap<EventId, Vec<HandlerSlot>>>,
    waterfall_handlers: RwLock<HashMap<EventId, Vec<WaterfallSlot>>>,
    bus: tokio::sync::broadcast::Sender<(EventId, serde_json::Value)>,
    /// Per-event dispatch counter (every mode, every dispatch path).
    dispatch_counts: Mutex<BTreeMap<String, u64>>,
}

impl EventsService {
    pub fn new() -> Self {
        let (tx, _rx) = tokio::sync::broadcast::channel(32);
        let svc = Self {
            handlers: RwLock::new(HashMap::new()),
            waterfall_handlers: RwLock::new(HashMap::new()),
            bus: tx,
            dispatch_counts: Mutex::new(BTreeMap::new()),
        };
        svc.register_default_admit_handler();
        svc
    }

    fn register_default_admit_handler(&self) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let slot = HandlerSlot {
            cancelled,
            options: EventOptions::default(),
            handler: Arc::new(|payload| Box::pin(async move { Ok(default_agent_admit(payload)) })),
        };
        self.handlers
            .write()
            .entry("agent.admit".into())
            .or_default()
            .push(slot);
    }

    /// Subscribe to the fire-and-forget emit broadcast bus.
    /// Snapshot of dispatch counters: (total, per-event sorted ascending).
    pub fn dispatch_snapshot(&self) -> (u64, Vec<(String, u64)>) {
        let map = self.dispatch_counts.lock();
        let total = map.values().sum();
        (total, map.iter().map(|(k, v)| (k.clone(), *v)).collect())
    }

    /// Subscribe to the fire-and-forget emit broadcast bus.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<(EventId, serde_json::Value)> {
        self.bus.subscribe()
    }

    /// Register a one-shot flat listener.
    ///
    /// The returned handle is the same early-cancel subscription [`Self::on`]
    /// yields; disposing it before the event ever fires unregisters the
    /// listener. Exactly-once is claimed AT INVOCATION through an atomic
    /// swap, so concurrent dispatches of the same event run the handler on
    /// exactly one task and every later dispatch observes an already-spent
    /// (skipped) slot. Delegates to [`Self::once_with`] with default options;
    /// a bail-chain-skipped listener stays registered until it actually runs
    /// (see [`Self::once_with`]).
    pub fn once<F, Fut>(&self, event: EventId, handler: F) -> Box<dyn Disposable>
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        self.once_with(event, EventOptions::default(), handler)
    }

    /// Register a one-shot flat listener with explicit [`EventOptions`].
    ///
    /// The claim/dispose flag discipline is identical to [`Self::once`];
    /// `options.prepend` controls the insertion position in the
    /// dispatch-order list, `options.global` marks the listener as exempt
    /// from context filters in [`Self::emit_filtered`]. The historical
    /// [`Self::once`] delegates here with default options.
    pub fn once_with<F, Fut>(
        &self,
        event: EventId,
        options: EventOptions,
        handler: F,
    ) -> Box<dyn Disposable>
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        debug_enforce_listener(&event, false);
        // One atomic flag does double duty: swapping it `true` AT INVOCATION
        // claims the single run among concurrent dispatches, and because it
        // IS the slot's cancellation flag, the spent slot is dropped by the
        // next dispatch's retain pass. A listener skipped by a bail chain is
        // never invoked, so its claim stays unspent and the slot stays
        // registered until a dispatch actually reaches and runs it.
        let claim = Arc::new(AtomicBool::new(false));
        let slot_flag = claim.clone();
        let handle_flag = claim.clone();
        // The claim wrapper as a `Handler`: each invocation first flips the
        // atomic; only the caller that observes `false` (the FIRST one)
        // runs the user handler. Cloning the Arc inside keeps the closure
        // `Fn` while handing an owned handle to the spawned future.
        let user = Arc::new(handler);
        let once_handler: Handler = {
            let user = user.clone();
            Arc::new(move |payload: serde_json::Value| {
                let claimed = claim.swap(true, Ordering::SeqCst);
                let user = user.clone();
                Box::pin(async move {
                    if claimed {
                        // Already spent: pass the payload through untouched.
                        return Ok(payload);
                    }
                    user(payload).await
                })
            })
        };
        let slot = HandlerSlot {
            cancelled: slot_flag,
            options,
            handler: once_handler,
        };
        self.insert_handler(event, options.prepend, slot);
        Box::new(move || {
            handle_flag.store(true, Ordering::SeqCst);
        })
    }

    pub fn on<F, Fut>(&self, event: EventId, handler: F) -> Box<dyn Disposable>
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        self.on_with(event, EventOptions::default(), handler)
    }

    /// Register a flat listener with explicit [`EventOptions`]: `prepend`
    /// inserts at the front of the dispatch-order list, `global` marks the
    /// listener as realm-agnostic for [`Self::emit_filtered`]. The
    /// historical [`Self::on`] delegates here with default options.
    pub fn on_with<F, Fut>(
        &self,
        event: EventId,
        options: EventOptions,
        handler: F,
    ) -> Box<dyn Disposable>
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        debug_enforce_listener(&event, false);
        let cancelled = Arc::new(AtomicBool::new(false));
        let slot = HandlerSlot {
            cancelled: cancelled.clone(),
            options,
            handler: Arc::new(move |v| Box::pin(handler(v))),
        };
        self.insert_handler(event, options.prepend, slot);
        Box::new(move || {
            cancelled.store(true, Ordering::SeqCst);
        })
    }

    /// Shared insertion point so `prepend` ordering is identical for
    /// `on_with` and `once_with` registrations.
    fn insert_handler(&self, event: EventId, prepend: bool, slot: HandlerSlot) {
        let mut handlers = self.handlers.write();
        let entry = handlers.entry(event).or_default();
        if prepend {
            entry.insert(0, slot);
        } else {
            entry.push(slot);
        }
    }

    /// Register a Cordis `waterfall` around-middleware handler.
    ///
    /// `handler` receives the current payload and a `next` continuation.  Calling
    /// `next(payload)` runs the downstream chain and yields its (possibly
    /// transformed) result; NOT calling `next` short-circuits the chain so later
    /// handlers do not run.  Handlers registered here are only invoked by
    /// [`dispatch`](EventsService::dispatch) with [`Dispatch::Waterfall`]; the plain
    /// [`on`](EventsService::on) registry is used for emit/parallel/serial/bail.
    pub fn on_waterfall<F, Fut>(&self, event: EventId, handler: F) -> Box<dyn Disposable>
    where
        F: Fn(serde_json::Value, WaterfallNext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        debug_enforce_listener(&event, true);
        let cancelled = Arc::new(AtomicBool::new(false));
        let slot = WaterfallSlot {
            cancelled: cancelled.clone(),
            handler: Arc::new(move |v, next| Box::pin(handler(v, next))),
        };
        let mut handlers = self.waterfall_handlers.write();
        let entry = handlers.entry(event).or_default();
        entry.push(slot);
        Box::new(move || {
            cancelled.store(true, Ordering::SeqCst);
        })
    }

    /// Snapshot active handlers for `event`, optionally excluding non-global
    /// listeners whose [`EventOptions`] fails `filter`. Global listeners are
    /// passed to the filter NEVER — they always participate, mirroring the
    /// reference-kernel `hook.global || !filter || filter.call(...)` clause.
    ///
    /// The retain pass runs under the same write guard as the unfiltered
    /// variant, so cancelled slots still drop here.
    fn active_handlers_filtered(
        &self,
        event: &EventId,
        filter: Option<&ListenerFilter>,
    ) -> Vec<Handler> {
        let Some(filter) = filter else {
            return self.active_handlers(event);
        };
        let mut handlers = self.handlers.write();
        let active = {
            let Some(slots) = handlers.get_mut(event) else {
                return Vec::new();
            };
            slots.retain(|slot| !slot.cancelled.load(Ordering::SeqCst));
            slots
                .iter()
                .filter(|slot| slot.options.global || filter(&slot.options))
                .map(|slot| slot.handler.clone())
                .collect::<Vec<_>>()
        };
        if active.is_empty() && handlers.get(event).is_some_and(Vec::is_empty) {
            handlers.remove(event);
        }
        active
    }

    fn active_handlers(&self, event: &EventId) -> Vec<Handler> {
        let mut handlers = self.handlers.write();
        let active = {
            let Some(slots) = handlers.get_mut(event) else {
                return Vec::new();
            };
            slots.retain(|slot| !slot.cancelled.load(Ordering::SeqCst));
            slots
                .iter()
                .map(|slot| slot.handler.clone())
                .collect::<Vec<_>>()
        };
        if active.is_empty() {
            handlers.remove(event);
        }
        active
    }

    fn active_waterfall(&self, event: &EventId) -> Vec<WaterfallHandler> {
        let mut handlers = self.waterfall_handlers.write();
        let active = {
            let Some(slots) = handlers.get_mut(event) else {
                return Vec::new();
            };
            slots.retain(|slot| !slot.cancelled.load(Ordering::SeqCst));
            slots
                .iter()
                .map(|slot| slot.handler.clone())
                .collect::<Vec<_>>()
        };
        if active.is_empty() {
            handlers.remove(event);
        }
        active
    }

    /// Filtered fire-and-forget emit: like [`Dispatch::Emit`] via
    /// [`Self::dispatch`], but non-global listeners are offered to `filter`
    /// first — a `false` verdict excludes the listener from this dispatch
    /// without unregistering it. Global listeners bypass the filter.
    ///
    /// The broadcast bus fan-out is NOT filtered (it has no listener
    /// metadata to filter on); only registered handlers participate in
    /// filtering. Returns null like every emit path.
    pub fn emit_filtered(
        &self,
        event: EventId,
        args: serde_json::Value,
        filter: ListenerFilter,
    ) -> Result<serde_json::Value, CordisError> {
        debug_enforce_dispatch(&event, Dispatch::Emit);
        *self
            .dispatch_counts
            .lock()
            .entry(event.to_string())
            .or_insert(0) += 1;
        let _ = self.bus.send((event.clone(), args.clone()));
        for h in self.active_handlers_filtered(&event, Some(&filter)) {
            let p = args.clone();
            tokio::spawn(async move {
                let _ = h(p).await;
            });
        }
        Ok(serde_json::Value::Null)
    }

    pub async fn dispatch(
        &self,
        event: EventId,
        payload: serde_json::Value,
        mode: Dispatch,
    ) -> Result<serde_json::Value, CordisError> {
        debug_enforce_dispatch(&event, mode);
        *self
            .dispatch_counts
            .lock()
            .entry(event.to_string())
            .or_insert(0) += 1;
        let handlers = self.active_handlers(&event);
        match mode {
            // Waterfall uses its own around-middleware registry. With no active
            // around handlers it is an identity operation.
            Dispatch::Waterfall => {
                let wf_handlers = self.active_waterfall(&event);
                if wf_handlers.is_empty() {
                    return Ok(payload);
                }
                run_waterfall_chain(wf_handlers, 0, payload, None).await
            }
            // Emit is fire-and-forget. The broadcast and spawned handlers do not
            // contribute a result, so callers always observe JSON null.
            Dispatch::Emit => {
                let _ = self.bus.send((event, payload.clone()));
                for h in handlers {
                    let p = payload.clone();
                    tokio::spawn(async move {
                        let _ = h(p).await;
                    });
                }
                Ok(serde_json::Value::Null)
            }
            // Parallel fans out the same payload to every handler. All tasks are
            // joined so errors are observed and no in-flight handler is dropped;
            // successful dispatch has no meaningful result and returns null.
            Dispatch::Parallel => {
                let mut set = tokio::task::JoinSet::new();
                for (name, h) in handlers.into_iter().enumerate() {
                    let p = payload.clone();
                    set.spawn(async move { (name, h(p).await) });
                }
                let mut failures: Vec<(String, String)> = Vec::new();
                while let Some(res) = set.join_next().await {
                    match res {
                        Err(join_err) => {
                            // Consume the JoinError exactly once: unwrap the
                            // panic payload when the task panicked, otherwise
                            // render the (returned) cancelled-task error.
                            let message = match join_err.try_into_panic() {
                                Ok(payload) => panic_payload_message(&payload),
                                Err(cancelled) => cancelled.to_string(),
                            };
                            failures.push(("listener-task".to_string(), message));
                        }
                        Ok((name, Err(err))) => {
                            failures.push((format!("listener[{name}]"), err.message()))
                        }
                        Ok((_, Ok(_))) => {}
                    }
                }
                if failures.is_empty() {
                    return Ok(serde_json::Value::Null);
                }
                tracing::warn!(
                    event = %event,
                    failures = %format_listener_errors(&failures),
                    "parallel dispatch collected listener failures"
                );
                Err(CordisError::Internal(format_listener_errors(&failures)))
            }
            // Serial invokes handlers in registration order with the original
            // payload. A non-null result bails out immediately; null means the
            // handler did not claim the event, so an all-null chain returns the
            // untouched original payload.
            Dispatch::Serial | Dispatch::Bail => run_bail_handlers(handlers, payload).await,
        }
    }

    /// Around-middleware waterfall whose terminal `next` is `core` rather than identity.
    ///
    /// Snapshot active waterfall handlers for `event`. With none registered, `core`
    /// runs immediately. Otherwise the same chain as [`dispatch`] with
    /// [`Dispatch::Waterfall`], except `index >= handlers.len()` invokes `core`
    /// instead of returning the payload unchanged. [`dispatch`] Waterfall stays
    /// identity-at-end.
    pub async fn waterfall_around<F, Fut>(
        &self,
        event: EventId,
        payload: serde_json::Value,
        core: F,
    ) -> Result<serde_json::Value, CordisError>
    where
        F: FnOnce(serde_json::Value) -> Fut + Send + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        debug_enforce_dispatch(&event, Dispatch::Waterfall);
        let handlers = self.active_waterfall(&event);
        if handlers.is_empty() {
            return core(payload).await;
        }
        let core: WaterfallCore = Box::new(move |p| {
            Box::pin(core(p))
                as Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>>
        });
        run_waterfall_chain(handlers, 0, payload, Some(core)).await
    }

    /// Typed dispatch: serialize the payload struct for `E`'s event and
    /// dispatch with the declared mode. Equivalent to
    /// [`dispatch`](EventsService::dispatch) with a pre-validated name/mode
    /// pair; serialization failure is a [`CordisError::Configuration`].
    pub async fn dispatch_typed<E: crate::events_payload::TypedEvent>(
        &self,
        payload: &E::Payload,
    ) -> Result<serde_json::Value, CordisError> {
        let value =
            serde_json::to_value(payload).map_err(|e| CordisError::Configuration(e.to_string()))?;
        self.dispatch(E::NAME.to_string(), value, E::MODE).await
    }

    /// Typed flat listener: the handler receives the deserialized payload
    /// struct instead of raw JSON.
    ///
    /// A payload that fails to deserialize is skipped with a warning and the
    /// incoming value passes through unchanged (for Serial/Bail chains this
    /// preserves pass-through semantics). Registration still goes through the
    /// same debug contract enforcement as [`on`](EventsService::on).
    pub fn on_typed<E, F, Fut>(&self, handler: F) -> Box<dyn Disposable>
    where
        E: crate::events_payload::TypedEvent,
        F: Fn(E::Payload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        debug_enforce_listener(&E::NAME.to_string(), E::AROUND);
        let wrapped = move |v: serde_json::Value| {
            let fut = match serde_json::from_value::<E::Payload>(v.clone()) {
                Ok(payload) => handler(payload),
                Err(err) => {
                    tracing::warn!(event = E::NAME, error = %err, "typed listener skipped malformed payload");
                    return Box::pin(async { Ok(v) })
                        as Pin<
                            Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>,
                        >;
                }
            };
            Box::pin(fut)
                as Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>>
        };
        self.on(E::NAME.to_string(), wrapped)
    }

    /// Typed around-middleware waterfall: the handler receives the
    /// deserialized payload struct plus the raw-JSON [`WaterfallNext`]
    /// continuation. The rest of the chain keeps working on serialized values;
    /// delegating handlers re-parse inside `next`, mirroring upstream TS where
    /// `next` carries serialized args.
    pub fn on_typed_waterfall<E, F, Fut>(&self, handler: F) -> Box<dyn Disposable>
    where
        E: crate::events_payload::TypedEvent,
        F: Fn(E::Payload, WaterfallNext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        debug_enforce_listener(&E::NAME.to_string(), E::AROUND);
        let wrapped = move |v: serde_json::Value, next: WaterfallNext| {
            let fut = match serde_json::from_value::<E::Payload>(v.clone()) {
                Ok(payload) => handler(payload, next),
                Err(err) => {
                    tracing::warn!(event = E::NAME, error = %err, "typed listener skipped malformed payload");
                    return Box::pin(async move {
                        // Preserve chain semantics: hand the untouched value to
                        // the continuation so downstream handlers still run.
                        next(v).await
                    })
                        as Pin<
                            Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>,
                        >;
                }
            };
            Box::pin(fut)
                as Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>>
        };
        self.on_waterfall(E::NAME.to_string(), wrapped)
    }
}

impl Default for EventsService {
    fn default() -> Self {
        Self::new()
    }
}

/// Best-effort message extraction from a panicked listener's panic payload,
/// used when a spawned parallel listener task panics before joining.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "listener panicked".to_string()
    }
}

fn json_u64(v: &serde_json::Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| {
        x.as_u64()
            .or_else(|| x.as_i64().and_then(|n| u64::try_from(n).ok()))
    })
}

/// Default `"agent.admit"` Bail handler. Deny JSON when payload counts fail
/// against payload quota fields. Enterprise (or missing quota fields) continues.
fn default_agent_admit(payload: serde_json::Value) -> serde_json::Value {
    if payload.get("tier").and_then(|v| v.as_str()) == Some("enterprise") {
        return serde_json::Value::Null;
    }
    let monthly = json_u64(&payload, "monthly").unwrap_or(0);
    let daily = json_u64(&payload, "daily").unwrap_or(0);
    let Some(rpm) = json_u64(&payload, "requests_per_month") else {
        return serde_json::Value::Null;
    };
    let Some(rpd) = json_u64(&payload, "requests_per_day") else {
        return serde_json::Value::Null;
    };
    if monthly >= rpm {
        return serde_json::json!({ "deny": "monthly" });
    }
    if daily >= rpd {
        return serde_json::json!({ "deny": "daily" });
    }
    serde_json::Value::Null
}

impl Service for EventsService {}

async fn run_bail_handlers(
    handlers: Vec<Handler>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, CordisError> {
    for handler in handlers {
        let result = handler(payload.clone()).await?;
        if !result.is_null() {
            return Ok(result);
        }
    }
    Ok(payload)
}

/// Optional terminal `next` for [`EventsService::waterfall_around`]. `None` is
/// identity (used by [`Dispatch::Waterfall`]).
type WaterfallCore = Box<
    dyn FnOnce(
            serde_json::Value,
        )
            -> Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>>
        + Send,
>;

/// Run a Cordis `waterfall` around-middleware chain starting at `index`.
///
/// Each handler receives the current payload and a `next` continuation.  The `next`
/// closure, when invoked, advances to `index + 1` (running the rest of the chain).
/// When the chain is exhausted, `core` runs if `Some`, otherwise the payload is
/// returned unchanged.  A handler that does not call `next` short-circuits: its
/// own return value is the final result, later handlers never run, and `core` is
/// dropped uncalled.  Errors propagate.
fn run_waterfall_chain(
    handlers: Vec<WaterfallHandler>,
    index: usize,
    payload: serde_json::Value,
    core: Option<WaterfallCore>,
) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>> {
    Box::pin(async move {
        if index >= handlers.len() {
            return match core {
                Some(core) => core(payload).await,
                None => Ok(payload),
            };
        }
        let handler = handlers[index].clone();
        // Build the continuation.  Because `next` is `FnOnce` and captures `index`,
        // each handler sees exactly one downstream step. `core` moves into `next`
        // so a short-circuit (unused `next`) skips the terminal function.
        let next = move |p: serde_json::Value| {
            let remaining = handlers.clone();
            Box::pin(async move { run_waterfall_chain(remaining, index + 1, p, core).await })
                as Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>>
        };
        handler(payload, Box::new(next)).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn on_dispose_unregisters_handler() {
        let svc = EventsService::new();
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let d = svc.on("gone".into(), move |_v| {
            let f = f.clone();
            async move {
                f.store(true, Ordering::SeqCst);
                Ok(serde_json::Value::Null)
            }
        });
        d.dispose();
        svc.dispatch("gone".into(), serde_json::json!({}), Dispatch::Emit)
            .await
            .unwrap();
        svc.dispatch("gone".into(), serde_json::json!({}), Dispatch::Serial)
            .await
            .unwrap();
        assert!(svc.handlers.read().get("gone").is_none());
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            !flag.load(Ordering::SeqCst),
            "disposed on() handler must not run for Emit or Serial"
        );
    }

    #[tokio::test]
    async fn on_waterfall_dispose_unregisters_handler() {
        let svc = EventsService::new();
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let d = svc.on_waterfall("gone.wf".into(), move |payload, _next| {
            let f = f.clone();
            async move {
                f.store(true, Ordering::SeqCst);
                Ok(payload)
            }
        });
        d.dispose();
        svc.dispatch(
            "gone.wf".into(),
            serde_json::json!({ "n": 1 }),
            Dispatch::Waterfall,
        )
        .await
        .unwrap();
        assert!(
            !flag.load(Ordering::SeqCst),
            "disposed on_waterfall handler must not run"
        );
        assert!(svc.waterfall_handlers.read().get("gone.wf").is_none());
    }

    /// Concurrent dispatches of the same event race the once-slot: exactly
    /// ONE invocation runs the handler, every other dispatch observes an
    /// already-claimed slot and passes through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn once_fires_exactly_once_concurrently() {
        let svc = std::sync::Arc::new(EventsService::new());
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r = runs.clone();
        svc.once("once.test".into(), move |payload| {
            let r = r.clone();
            async move {
                r.fetch_add(1, Ordering::SeqCst);
                Ok(payload)
            }
        });

        // Fire N overlapping Parallel dispatches; each fans out to the same
        // slot concurrently.
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let svc = std::sync::Arc::clone(&svc);
            tasks.spawn(async move {
                svc.dispatch(
                    "once.test".into(),
                    serde_json::json!({}),
                    Dispatch::Parallel,
                )
                .await
            });
        }
        while let Some(res) = tasks.join_next().await {
            res.expect("dispatch task").expect("parallel dispatch ok");
        }
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "exactly one concurrent dispatch may run the once handler"
        );
    }

    /// A bail-chain skip is NOT a run: a once listener that never got to run
    /// because an earlier handler bailed stays registered until it actually
    /// executes on a later dispatch.
    #[tokio::test]
    async fn once_stays_registered_when_skipped_by_bail() {
        let svc = EventsService::new();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first = ran.clone();
        // Earlier handler bails with a non-null result: the chain stops here.
        let bailer = svc.on("once.bail".into(), move |_payload| {
            let first = first.clone();
            async move {
                first.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({ "handled": true }))
            }
        });
        let second = ran.clone();
        svc.once("once.bail".into(), move |payload| {
            let second = second.clone();
            async move {
                second.fetch_add(1, Ordering::SeqCst);
                Ok(payload)
            }
        });

        // Dispatch 1: bail handler claims; the once listener is skipped
        // WITHOUT running (its claim must stay unspent).
        let out = svc
            .dispatch("once.bail".into(), serde_json::json!({}), Dispatch::Bail)
            .await
            .unwrap();
        assert_eq!(out, serde_json::json!({ "handled": true }));
        assert_eq!(ran.load(Ordering::SeqCst), 1, "only the bail handler ran");
        // Slot is still registered (never fired).
        assert!(svc.handlers.read().get("once.bail").is_some());

        // Dispatch 2: the bail handler terminates the chain again — the once
        // listener is skipped a second time and REMAINS registered.
        let _ = svc
            .dispatch("once.bail".into(), serde_json::json!({}), Dispatch::Bail)
            .await;
        assert_eq!(
            ran.load(Ordering::SeqCst),
            2,
            "the bail handler claims every chain"
        );
        assert!(
            svc.handlers.read().get("once.bail").is_some(),
            "skipped-by-bail once slot stays registered"
        );

        // Dispose ONLY the bailer's own subscription so the next chain
        // actually REACHES the pending once slot (same service instance,
        // same slot — nothing was re-registered).
        bailer.dispose();
        let out = svc
            .dispatch(
                "once.bail".into(),
                serde_json::json!({"n": 1}),
                Dispatch::Bail,
            )
            .await
            .unwrap();
        // No bailer left: the once listener runs (chain ends null → payload).
        assert_eq!(out, serde_json::json!({"n": 1}));
        assert_eq!(
            ran.load(Ordering::SeqCst),
            3,
            "the surviving once slot fires exactly once"
        );
        // One more dispatch prunes the spent slot from the registry AND
        // proves it cannot run again.
        let _ = svc
            .dispatch(
                "once.bail".into(),
                serde_json::json!({"n": 2}),
                Dispatch::Bail,
            )
            .await;
        assert_eq!(
            ran.load(Ordering::SeqCst),
            3,
            "spent once slot must not run again"
        );
        assert!(
            svc.handlers.read().get("once.bail").is_none(),
            "pruned from the registry after spending"
        );
    }

    #[tokio::test]
    async fn parallel_returns_null_after_all_handlers_complete() {
        let svc = EventsService::new();
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for value in [1, 2] {
            let completed = completed.clone();
            svc.on("parallel.result".into(), move |_payload| {
                let completed = completed.clone();
                async move {
                    completed.fetch_add(value, Ordering::SeqCst);
                    Ok(serde_json::json!({ "value": value }))
                }
            });
        }

        let out = svc
            .dispatch(
                "parallel.result".into(),
                serde_json::json!({ "input": true }),
                Dispatch::Parallel,
            )
            .await
            .unwrap();

        assert_eq!(out, serde_json::Value::Null);
        assert_eq!(completed.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn serial_stops_at_first_non_null_result() {
        let svc = EventsService::new();
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first = ran.clone();
        svc.on("serial.bail".into(), move |payload| {
            let first = first.clone();
            async move {
                first.fetch_add(1, Ordering::SeqCst);
                assert_eq!(payload["input"], true);
                Ok(serde_json::Value::Null)
            }
        });
        let second = ran.clone();
        svc.on("serial.bail".into(), move |payload| {
            let second = second.clone();
            async move {
                second.fetch_add(1, Ordering::SeqCst);
                assert_eq!(payload["input"], true);
                Ok(serde_json::json!({ "handled": true }))
            }
        });
        let third = ran.clone();
        svc.on("serial.bail".into(), move |_payload| {
            let third = third.clone();
            async move {
                third.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({ "late": true }))
            }
        });

        let out = svc
            .dispatch(
                "serial.bail".into(),
                serde_json::json!({ "input": true }),
                Dispatch::Serial,
            )
            .await
            .unwrap();

        assert_eq!(out, serde_json::json!({ "handled": true }));
        assert_eq!(ran.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn serial_preserves_original_payload_when_no_handler_bails() {
        let svc = EventsService::new();
        svc.on("serial.identity".into(), |_payload| async move {
            Ok(serde_json::Value::Null)
        });
        svc.on("serial.identity".into(), |_payload| async move {
            Ok(serde_json::Value::Null)
        });

        let payload = serde_json::json!({ "input": [1, 2], "nested": { "ok": true } });
        let out = svc
            .dispatch("serial.identity".into(), payload.clone(), Dispatch::Serial)
            .await
            .unwrap();

        assert_eq!(out, payload);
    }

    #[tokio::test]
    async fn default_agent_admit_handler_denies_monthly() {
        let svc = EventsService::new();
        let out = svc
            .dispatch(
                "agent.admit".into(),
                serde_json::json!({
                    "monthly": 10,
                    "daily": 0,
                    "requests_per_month": 10,
                    "requests_per_day": 50,
                    "tier": "free"
                }),
                Dispatch::Bail,
            )
            .await
            .unwrap();
        assert_eq!(out["deny"], "monthly");
    }

    #[tokio::test]
    async fn default_agent_admit_handler_allows_under_quota() {
        let svc = EventsService::new();
        let out = svc
            .dispatch(
                "agent.admit".into(),
                serde_json::json!({
                    "monthly": 0,
                    "daily": 0,
                    "requests_per_month": 10,
                    "requests_per_day": 50,
                    "tier": "free"
                }),
                Dispatch::Bail,
            )
            .await
            .unwrap();
        assert!(
            out.get("deny").is_none(),
            "under-quota must not deny, got {out}"
        );
    }

    #[tokio::test]
    async fn waterfall_around_no_handlers_runs_core() {
        let svc = EventsService::new();
        let out = svc
            .waterfall_around(
                "around.empty".into(),
                serde_json::json!({}),
                |mut payload| async move {
                    if let Some(obj) = payload.as_object_mut() {
                        obj.insert("core".into(), serde_json::json!(true));
                    }
                    Ok(payload)
                },
            )
            .await
            .unwrap();
        assert_eq!(out["core"], true);
    }

    #[tokio::test]
    async fn waterfall_around_handler_calls_next_then_core() {
        let svc = EventsService::new();
        svc.on_waterfall("around.wrap".into(), |mut payload, next| async move {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("wrap".into(), serde_json::json!(true));
            }
            next(payload).await
        });
        let out = svc
            .waterfall_around(
                "around.wrap".into(),
                serde_json::json!({}),
                |mut payload| async move {
                    if let Some(obj) = payload.as_object_mut() {
                        obj.insert("core".into(), serde_json::json!(true));
                    }
                    Ok(payload)
                },
            )
            .await
            .unwrap();
        assert_eq!(out["wrap"], true);
        assert_eq!(out["core"], true);
    }

    #[tokio::test]
    async fn waterfall_around_short_circuit_skips_core() {
        let svc = EventsService::new();
        let flag = Arc::new(AtomicBool::new(false));
        svc.on_waterfall("around.short".into(), |payload, _next| async move {
            Ok(payload)
        });
        let f = flag.clone();
        let out = svc
            .waterfall_around(
                "around.short".into(),
                serde_json::json!({ "ok": true }),
                move |payload| {
                    let f = f.clone();
                    async move {
                        f.store(true, Ordering::SeqCst);
                        Ok(payload)
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        assert!(
            !flag.load(Ordering::SeqCst),
            "core must not run when handler short-circuits"
        );
    }

    // -- typed wrappers (events_payload) -----------------------------------

    /// dispatch_typed -> on_typed round trip on an Emit event: the handler
    /// observes the deserialized struct, not raw JSON.
    #[tokio::test]
    async fn typed_dispatch_and_listener_round_trip() {
        let svc = EventsService::new();
        let seen = Arc::new(parking_lot::Mutex::new(
            None::<crate::events_payload::AgentUsagePayload>,
        ));
        let slot = seen.clone();
        let _d = svc.on_typed::<crate::events_payload::AgentUsageEvent, _, _>(move |p| {
            let slot = slot.clone();
            async move {
                *slot.lock() = Some(p);
                Ok(serde_json::Value::Null)
            }
        });
        svc.dispatch_typed::<crate::events_payload::AgentUsageEvent>(
            &crate::events_payload::AgentUsagePayload {
                tenant: Some("acme".into()),
                prompt: 3,
                completion: 4,
                total: 7,
            },
        )
        .await
        .unwrap();
        // Emit spawns handlers; poll briefly for the handler to land.
        for _ in 0..100 {
            if seen.lock().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let got = seen.lock().clone().expect("handler must observe payload");
        assert_eq!(got.tenant.as_deref(), Some("acme"));
        assert_eq!(got.total, 7);
    }

    /// A malformed payload skips the typed handler and passes the value
    /// through unchanged (Bail passthrough semantics preserved).
    #[tokio::test]
    async fn typed_listener_skips_malformed_payload_passthrough() {
        let svc = EventsService::new();
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let _d = svc.on_typed::<crate::events_payload::AgentAdmitEvent, _, _>(move |_p| {
            let flag = flag.clone();
            async move {
                flag.store(true, Ordering::SeqCst);
                Ok(serde_json::json!({ "deny": "daily" }))
            }
        });
        let out = svc
            .dispatch(
                crate::events_catalog::ev::AGENT_ADMIT.into(),
                serde_json::json!({ "tenant_id": 42 }), // wrong type: not a string
                Dispatch::Bail,
            )
            .await
            .unwrap();
        assert!(
            !ran.load(Ordering::SeqCst),
            "malformed payload must skip the typed handler"
        );
        assert_eq!(out["tenant_id"], 42, "value must pass through unchanged");
    }

    /// Typed waterfall listener can short-circuit by not calling next; the
    /// returned value is the chain result.
    #[tokio::test]
    async fn typed_waterfall_short_circuit() {
        use crate::events::WaterfallNext;
        let svc = EventsService::new();
        let _d = svc.on_typed_waterfall::<crate::events_payload::LlmGetClientEvent, _, _>(
            |p, next: WaterfallNext| async move {
                if p.capability == "blocked" {
                    return Ok(serde_json::json!({ "deny": true }));
                }
                next(serde_json::json!({ "capability": p.capability })).await
            },
        );
        let denied = svc
            .dispatch(
                crate::events_catalog::ev::LLM_GET_CLIENT.into(),
                serde_json::json!({ "capability": "blocked" }),
                Dispatch::Waterfall,
            )
            .await
            .unwrap();
        assert_eq!(denied["deny"], true);
        let passed = svc
            .dispatch(
                crate::events_catalog::ev::LLM_GET_CLIENT.into(),
                serde_json::json!({ "capability": "chat" }),
                Dispatch::Waterfall,
            )
            .await
            .unwrap();
        assert_eq!(passed["capability"], "chat");
    }

    #[test]
    fn summarize_listener_errors_formats_multiple_failures() {
        let summary = crate::events::summarize_listener_errors(vec![
            ("quota".to_string(), "monthly cap reached".to_string()),
            ("audit".to_string(), "db write failed".to_string()),
        ]);
        assert_eq!(
            summary,
            "2 listener failures: quota: monthly cap reached; audit: db write failed"
        );
    }

    #[test]
    fn summarize_listener_errors_formats_single_failure() {
        let summary = crate::events::summarize_listener_errors(vec![(
            "solo".to_string(),
            "boom".to_string(),
        )]);
        assert_eq!(summary, "1 listener failure: solo: boom");
        assert_eq!(
            crate::events::summarize_listener_errors(Vec::new()),
            "0 listener failures"
        );
    }

    #[test]
    fn aggregate_error_display_and_error_impl() {
        let agg = AggregateError {
            errors: vec![
                ("a".to_string(), "x".to_string()),
                ("b".to_string(), "y".to_string()),
            ],
        };
        assert_eq!(agg.to_string(), "2 listener failures: a: x; b: y");
        // std::error::Error is object-safe usable via dyn.
        let dyn_err: &dyn std::error::Error = &agg;
        assert!(dyn_err.to_string().contains("b: y"));
    }

    /// Parallel dispatch aggregates EVERY failed listener (not just the first)
    /// into one Internal error whose message is the shared summary format;
    /// successes still run to completion and the failure names each listener
    /// position with its message.
    #[tokio::test]
    async fn parallel_dispatch_aggregates_all_listener_failures() {
        use crate::CordisError as Err;
        let svc = EventsService::new();
        let completed = Arc::new(AtomicBool::new(false));
        let c = completed.clone();
        svc.on("par.agg".into(), move |_p| {
            let c = c.clone();
            async move {
                c.store(true, Ordering::SeqCst);
                Ok(serde_json::json!({ "ok": true }))
            }
        });
        svc.on("par.agg".into(), |_p| async move {
            Err::<serde_json::Value, _>(Err::Configuration("first failure".into()))
        });
        svc.on("par.agg".into(), |_p| async move {
            Err::<serde_json::Value, _>(Err::Fiber("second failure".into()))
        });

        let err = svc
            .dispatch("par.agg".into(), serde_json::json!({}), Dispatch::Parallel)
            .await
            .unwrap_err();
        let text = err.message();
        assert!(
            text.starts_with("internal kernel error: 2 listener failures:")
                && text.contains("listener[1]: configuration error: first failure")
                && text.contains("listener[2]: fiber error: second failure"),
            "aggregate must list every failing listener, got: {text}"
        );
        assert!(
            completed.load(Ordering::SeqCst),
            "healthy listener still ran"
        );
    }

    /// A single failing listener yields the singular summary wording through
    /// the same dispatch path.
    #[tokio::test]
    async fn parallel_dispatch_single_failure_uses_singular_summary() {
        let svc = EventsService::new();
        svc.on("par.solo".into(), |_p| async move {
            Err::<serde_json::Value, _>(crate::CordisError::Internal("only one".into()))
        });
        let err = svc
            .dispatch("par.solo".into(), serde_json::json!({}), Dispatch::Parallel)
            .await
            .unwrap_err();
        assert_eq!(
            err.message(),
            "internal kernel error: 1 listener failure: listener[0]: internal kernel error: only one"
        );
    }

    // ------------------------------------------------------------------
    // EventOptions: prepend ordering + global filter bypass (C1)
    // ------------------------------------------------------------------

    /// `on_with(prepend)` runs the prepended listener BEFORE previously
    /// registered listeners of the same event; default registration keeps
    /// appending. Proved with a Serial dispatch whose run order is recorded.
    #[tokio::test]
    async fn prepend_ordering_observed() {
        let svc = EventsService::new();
        let order = Arc::new(parking_lot::Mutex::<Vec<String>>::new(Vec::new()));

        for name in ["first", "second"] {
            let slot = order.clone();
            svc.on("prepend.test".into(), move |_p| {
                let slot = slot.clone();
                async move {
                    slot.lock().push(name.to_string());
                    Ok(serde_json::Value::Null)
                }
            });
        }

        // Prepended persistent listener: must land in FRONT of both defaults.
        let prepended_slot = order.clone();
        svc.on_with(
            "prepend.test".into(),
            EventOptions {
                prepend: true,
                global: false,
            },
            move |_p| {
                let prepended_slot = prepended_slot.clone();
                async move {
                    prepended_slot.lock().push("prepended".to_string());
                    Ok(serde_json::Value::Null)
                }
            },
        );

        // Also prove the once_with path honors prepend: it must land in front.
        let once_slot = order.clone();
        svc.once_with(
            "prepend.test".into(),
            EventOptions {
                prepend: true,
                global: false,
            },
            move |_p| {
                let once_slot = once_slot.clone();
                async move {
                    once_slot.lock().push("once-prepended".to_string());
                    Ok(serde_json::Value::Null)
                }
            },
        );

        svc.dispatch(
            "prepend.test".into(),
            serde_json::json!({}),
            Dispatch::Serial,
        )
        .await
        .unwrap();
        assert_eq!(
            *order.lock(),
            ["once-prepended", "prepended", "first", "second"],
            "prepend inserts at the dispatch-order front; defaults append"
        );

        // A second Serial pass re-runs only the persistent listeners, in the
        // same relative order (the once slot is spent).
        svc.dispatch(
            "prepend.test".into(),
            serde_json::json!({}),
            Dispatch::Serial,
        )
        .await
        .unwrap();
        assert_eq!(
            order.lock()[4..],
            ["prepended", "first", "second"],
            "spent once slot drops out; relative order is stable"
        );
    }

    /// `emit_filtered` excludes non-global listeners whose options fail the
    /// filter and runs the rest — without unregistering anyone: an
    /// unfiltered dispatch afterwards runs every listener again.
    #[tokio::test]
    async fn filter_excludes_nonmatching_contexts() {
        let svc = EventsService::new();
        // One counter per listener, incremented on EVERY run so each
        // dispatch's participation is directly observable.
        let ran_a = Arc::new(AtomicUsize::new(0));
        let ran_b = Arc::new(AtomicUsize::new(0));

        let a = ran_a.clone();
        svc.on_with("filtered.test".into(), EventOptions::default(), move |p| {
            let a = a.clone();
            async move {
                a.fetch_add(1, Ordering::SeqCst);
                Ok(p)
            }
        });
        let b = ran_b.clone();
        svc.on_with("filtered.test".into(), EventOptions::default(), move |p| {
            let b = b.clone();
            async move {
                b.fetch_add(1, Ordering::SeqCst);
                Ok(p)
            }
        });

        // Filter admits NOTHING: neither listener runs.
        svc.emit_filtered(
            "filtered.test".into(),
            serde_json::json!({ "tenant": "a" }),
            Box::new(|_opts| false),
        )
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(ran_a.load(Ordering::SeqCst), 0, "rejecting filter excludes a");
        assert_eq!(ran_b.load(Ordering::SeqCst), 0, "rejecting filter excludes b");

        // Exclusion was per-dispatch: an UNFILTERED emit runs both listeners.
        svc.dispatch("filtered.test".into(), serde_json::json!({}), Dispatch::Emit)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(
            ran_a.load(Ordering::SeqCst),
            1,
            "listener a must be back for unfiltered dispatches"
        );
        assert_eq!(
            ran_b.load(Ordering::SeqCst),
            1,
            "listener b must be back for unfiltered dispatches"
        );

        // And both registration slots survived the filtered pass untouched.
        let handlers = svc.handlers.read();
        let slots = handlers.get("filtered.test").expect("entry kept");
        assert_eq!(
            slots.len(),
            2,
            "filter exclusion must not unregister anyone"
        );
    }

    /// Global listeners bypass context filters entirely: the same
    /// `emit_filtered` that excludes a non-global listener leaves a global
    /// one untouched by the filter verdict.
    #[tokio::test]
    async fn global_bypasses_filter() {
        let svc = EventsService::new();
        let ran = Arc::new(AtomicUsize::new(0));

        // Non-global listener registered for tenant "b".
        let b = ran.clone();
        svc.on_with(
            "global.test".into(),
            EventOptions::default(),
            move |payload| {
                let b = b.clone();
                async move {
                    if payload["tenant"] == "b" {
                        b.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(payload)
                }
            },
        );

        // Global listener for tenant "b": exempt from every filter.
        let g = ran.clone();
        svc.on_with(
            "global.test".into(),
            EventOptions {
                prepend: false,
                global: true,
            },
            move |payload| {
                let g = g.clone();
                async move {
                    if payload["tenant"] == "b" {
                        g.fetch_add(10, Ordering::SeqCst);
                    }
                    Ok(payload)
                }
            },
        );

        // Dispatch under a filter that admits NOTHING ("tenant z"): the
        // non-global listener is excluded, the global one still runs.
        svc.emit_filtered(
            "global.test".into(),
            serde_json::json!({ "tenant": "b" }),
            Box::new(|_opts| false),
        )
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(
            ran.load(Ordering::SeqCst),
            10,
            "global listener runs despite a rejecting filter; non-global does not"
        );
    }
}
