use parking_lot::RwLock;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::effect::Disposable;
use crate::service::{CordisError, Service};
use crate::EventId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Emit,
    Parallel,
    Serial,
    Bail,
    Waterfall,
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
}

impl EventsService {
    pub fn new() -> Self {
        let (tx, _rx) = tokio::sync::broadcast::channel(32);
        let svc = Self {
            handlers: RwLock::new(HashMap::new()),
            waterfall_handlers: RwLock::new(HashMap::new()),
            bus: tx,
        };
        svc.register_default_admit_handler();
        svc
    }

    fn register_default_admit_handler(&self) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let slot = HandlerSlot {
            cancelled,
            handler: Arc::new(|payload| Box::pin(async move { Ok(default_agent_admit(payload)) })),
        };
        self.handlers
            .write()
            .entry("agent.admit".into())
            .or_default()
            .push(slot);
    }

    /// Subscribe to the fire-and-forget emit broadcast bus.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<(EventId, serde_json::Value)> {
        self.bus.subscribe()
    }

    pub fn on<F, Fut>(&self, event: EventId, handler: F) -> Box<dyn Disposable>
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        let cancelled = Arc::new(AtomicBool::new(false));
        let slot = HandlerSlot {
            cancelled: cancelled.clone(),
            handler: Arc::new(move |v| Box::pin(handler(v))),
        };
        let mut handlers = self.handlers.write();
        let entry = handlers.entry(event).or_default();
        entry.push(slot);
        Box::new(move || {
            cancelled.store(true, Ordering::SeqCst);
        })
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

    pub async fn dispatch(
        &self,
        event: EventId,
        payload: serde_json::Value,
        mode: Dispatch,
    ) -> Result<serde_json::Value, CordisError> {
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
                for h in handlers {
                    let p = payload.clone();
                    set.spawn(async move { h(p).await });
                }
                let mut first_error = None;
                while let Some(res) = set.join_next().await {
                    match res {
                        Err(join_err) => {
                            if first_error.is_none() {
                                first_error = Some(CordisError::Fiber(join_err.to_string()));
                            }
                        }
                        Ok(Err(err)) => {
                            if first_error.is_none() {
                                first_error = Some(err);
                            }
                        }
                        Ok(Ok(_)) => {}
                    }
                }
                match first_error {
                    Some(err) => Err(err),
                    None => Ok(serde_json::Value::Null),
                }
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
}

impl Default for EventsService {
    fn default() -> Self {
        Self::new()
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
}
