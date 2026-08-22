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
    dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>>
        + Send
        + Sync,
>;

/// The `next` continuation handed to a waterfall handler.  It advances to the
/// next registered waterfall handler, or returns the passed payload unchanged once
/// the chain is exhausted.  It is `FnOnce`: a handler may call `next` at most once,
/// mirroring Cordis `next()` semantics.
pub type WaterfallNext =
    Box<dyn FnOnce(serde_json::Value) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>> + Send>;

/// A Cordis `waterfall` around-middleware handler.  It receives the current payload
/// plus a `next` continuation.  Calling `next(payload)` runs the downstream chain and
/// yields its result for further transformation; choosing NOT to call `next`
/// short-circuits the chain (any later handlers do not run).
type WaterfallHandler = Arc<
    dyn Fn(serde_json::Value, WaterfallNext)
        -> Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>>
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
        Self {
            handlers: RwLock::new(HashMap::new()),
            waterfall_handlers: RwLock::new(HashMap::new()),
            bus: tx,
        }
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
        self.handlers
            .read()
            .get(event)
            .map(|slots| {
                slots
                    .iter()
                    .filter(|s| !s.cancelled.load(Ordering::SeqCst))
                    .map(|s| s.handler.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn active_waterfall(&self, event: &EventId) -> Vec<WaterfallHandler> {
        self.waterfall_handlers
            .read()
            .get(event)
            .map(|slots| {
                slots
                    .iter()
                    .filter(|s| !s.cancelled.load(Ordering::SeqCst))
                    .map(|s| s.handler.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn dispatch(
        &self,
        event: EventId,
        payload: serde_json::Value,
        mode: Dispatch,
    ) -> Result<serde_json::Value, CordisError> {
        let handlers = self.active_handlers(&event);
        match mode {
            // Cordis `waterfall` uses its own around-middleware registry, so it
            // must not be short-circuited by the plain-handler emptiness check.
            Dispatch::Waterfall => {
                let wf_handlers = self.active_waterfall(&event);
                if wf_handlers.is_empty() {
                    return Ok(payload);
                }
                run_waterfall_chain(wf_handlers, 0, payload).await
            }
            _ if handlers.is_empty() => Ok(payload),
            // Cordis `emit`: fire-and-forget. Every handler is spawned and NOT
            // awaited, so `dispatch` returns immediately; the event+payload is
            // also broadcast on the bus. Handlers still run to completion on the
            // runtime (a caller that needs to observe completion should listen on
            // the bus or use a oneshot/notify channel instead of awaiting this).
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
            // Cordis `parallel`: run every handler concurrently (fan-out); if any
            // handler errors, propagate the first error we observe.
            Dispatch::Parallel => {
                let mut set = tokio::task::JoinSet::new();
                for h in handlers {
                    let p = payload.clone();
                    set.spawn(async move { h(p).await });
                }
                let mut last = serde_json::Value::Null;
                while let Some(res) = set.join_next().await {
                    match res {
                        // Task panicked — surface as a fiber error.
                        Err(join_err) => return Err(CordisError::Fiber(join_err.to_string())),
                        Ok(Err(e)) => return Err(e),
                        Ok(Ok(v)) => last = v,
                    }
                }
                Ok(last)
            }
            // Cordis `serial`: thread the payload through each handler in order;
            // a handler error aborts the chain and propagates.
            Dispatch::Serial => {
                let mut cur = payload;
                for h in handlers {
                    cur = h(cur).await?;
                }
                Ok(cur)
            }
            // Cordis `bail`: stop at the first handler that returns a non-null
            // result (`isBailed` analog) and return that value without running
            // any later handlers. A null result means "not bailing" — the chain
            // continues with the original payload.
            Dispatch::Bail => {
                let cur = payload;
                for h in handlers {
                    let res = h(cur.clone()).await?;
                    if !res.is_null() {
                        return Ok(res);
                    }
                }
                Ok(cur)
            }
        }
    }
}

impl Default for EventsService {
    fn default() -> Self {
        Self::new()
    }
}

impl Service for EventsService {}

/// Run a Cordis `waterfall` around-middleware chain starting at `index`.
///
/// Each handler receives the current payload and a `next` continuation.  The `next`
/// closure, when invoked, advances to `index + 1` (running the rest of the chain)
/// or returns the given payload unchanged when the chain is exhausted.  A handler
/// that does not call `next` short-circuits: its own return value is the final
/// result and later handlers never run.  Errors propagate.
fn run_waterfall_chain(
    handlers: Vec<WaterfallHandler>,
    index: usize,
    payload: serde_json::Value,
) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>> {
    Box::pin(async move {
        if index >= handlers.len() {
            return Ok(payload);
        }
        let handler = handlers[index].clone();
        // Build the continuation.  Because `next` is `FnOnce` and captures `index`,
        // each handler sees exactly one downstream step.
        let next = move |p: serde_json::Value| {
            let remaining = handlers.clone();
            Box::pin(async move { run_waterfall_chain(remaining, index + 1, p).await })
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
        svc.dispatch("gone.wf".into(), serde_json::json!({ "n": 1 }), Dispatch::Waterfall)
            .await
            .unwrap();
        assert!(
            !flag.load(Ordering::SeqCst),
            "disposed on_waterfall handler must not run"
        );
    }
}
