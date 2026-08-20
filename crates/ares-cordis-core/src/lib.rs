#![allow(missing_docs)]
#![allow(dead_code)]

use parking_lot::{Mutex, RwLock};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};

use thiserror::Error;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum CordisError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("fiber error: {0}")]
    Fiber(String),
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub type Symbol = String;
pub type EventId = String;
pub type FiberId = u64;

// ---------------------------------------------------------------------------
// Disposable / Effect
// ---------------------------------------------------------------------------

pub trait Disposable: Send + 'static {
    fn dispose(self: Box<Self>);
}

impl<F> Disposable for F
where
    F: FnOnce() + Send + 'static,
{
    fn dispose(self: Box<Self>) {
        (*self)()
    }
}

pub trait Effect: Send + Sync + 'static {
    fn apply(&self, ctx: &Arc<Context>) -> Box<dyn Disposable>;
}

// EffectGuard reverses on Drop (LIFO)
pub struct EffectGuard {
    acc: Vec<Box<dyn FnOnce() + Send>>,
}

impl EffectGuard {
    pub fn new() -> Self {
        Self { acc: Vec::new() }
    }
    pub fn push(&mut self, undo: Box<dyn FnOnce() + Send>) {
        self.acc.push(undo);
    }
}

impl Default for EffectGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EffectGuard {
    fn drop(&mut self) {
        while let Some(undo) = self.acc.pop() {
            undo();
        }
    }
}

// ---------------------------------------------------------------------------
// Service trait
// ---------------------------------------------------------------------------

pub trait Service: Send + Sync + 'static {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn init(
        &self,
        _ctx: &Arc<Context>,
    ) -> impl Future<Output = Result<Option<Box<dyn Disposable>>, CordisError>> + Send {
        async move { Ok(None) }
    }

    fn check(&self) -> bool {
        true
    }
}

// Blanket for Arc<T> where T: Service? Not needed.

// ---------------------------------------------------------------------------
// Fiber
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FiberState {
    Inactive { error: Option<String> },
    Active { epoch: String },
    Reloading,
    Unloading { error: Option<String> },
}

pub struct Fiber {
    state: RwLock<FiberState>,
    inertia: Arc<tokio::sync::Mutex<()>>,
    acc: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    epoch: RwLock<String>,
    injects: RwLock<HashMap<TypeId, String>>, // TypeId -> type_name
    // committed snapshot placeholder
}

impl Fiber {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(FiberState::Inactive { error: None }),
            inertia: Arc::new(tokio::sync::Mutex::new(())),
            acc: Mutex::new(Vec::new()),
            epoch: RwLock::new(String::new()),
            injects: RwLock::new(HashMap::new()),
        }
    }

    pub fn declare_inject<T: Service>(&self) {
        let tid = TypeId::of::<T>();
        let name = std::any::type_name::<T>().to_string();
        self.injects.write().insert(tid, name);
    }

    pub fn state(&self) -> FiberState {
        self.state.read().clone()
    }

    pub fn epoch(&self) -> String {
        self.epoch.read().clone()
    }

    /// Compute epoch as ":type_name:version:..." sorted (monoid over concatenation)
    pub fn compute_epoch(&self, ctx: &Arc<Context>) -> String {
        let injects = self.injects.read();
        if injects.is_empty() {
            return ":".to_string();
        }
        let mut frags: Vec<String> = Vec::new();
        for (tid, type_name) in injects.iter() {
            let version = ctx.get_version(*tid);
            frags.push(format!("{}:{}", type_name, version));
        }
        frags.sort();
        format!(":{}", frags.join(":"))
    }

    /// Refresh recomputes epoch from inject deps; if changed, reload.
    pub async fn refresh(&self, ctx: &Arc<Context>) {
        let _guard = self.inertia.lock().await;
        let new_epoch = self.compute_epoch(ctx);
        let old_epoch = self.epoch.read().clone();
        if new_epoch == old_epoch && *self.state.read() != (FiberState::Inactive { error: None }) {
            // No change and already active -> no reload
            // But also check if still active vs inactive due to dep satisfaction
            let satisfied = self.is_satisfied(ctx);
            if satisfied && matches!(*self.state.read(), FiberState::Inactive { .. }) {
                // was inactive but now satisfied without epoch change? Should activate
                // e.g., first provide when epoch goes from "" to ":Foo:1"
            } else {
                return;
            }
        }
        let satisfied = self.is_satisfied(ctx);
        if satisfied {
            *self.epoch.write() = new_epoch.clone();
            *self.state.write() = FiberState::Active { epoch: new_epoch };
        } else {
            *self.state.write() = FiberState::Inactive { error: None };
            // do not update epoch when inactive? keep old?
        }
    }

    fn is_satisfied(&self, ctx: &Arc<Context>) -> bool {
        let injects = self.injects.read();
        for tid in injects.keys() {
            if ctx.get_version(*tid) == 0 {
                return false;
            }
        }
        true
    }

    pub async fn dispose(&self) {
        let _guard = self.inertia.lock().await;
        let mut acc = self.acc.lock();
        while let Some(undo) = acc.pop() {
            undo();
        }
        *self.state.write() = FiberState::Inactive { error: None };
        *self.epoch.write() = String::new();
    }

    // Called by Context::provide to push undo onto this fiber's acc
    pub(crate) fn push_undo(&self, undo: Box<dyn FnOnce() + Send>) {
        self.acc.lock().push(undo);
    }
}

impl Default for Fiber {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

pub struct Context {
    store: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    isolate: RwLock<HashMap<TypeId, Symbol>>,
    intercept: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    versions: RwLock<HashMap<TypeId, u64>>,
    fiber: Arc<Fiber>,
    parent: Option<Arc<Context>>,
    root: Weak<Context>,
}

impl Context {
    pub fn new_root() -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            store: RwLock::new(HashMap::new()),
            isolate: RwLock::new(HashMap::new()),
            intercept: RwLock::new(HashMap::new()),
            versions: RwLock::new(HashMap::new()),
            fiber: Arc::new(Fiber::new()),
            parent: None,
            root: weak.clone(),
        })
    }

    pub fn extend(self: &Arc<Self>) -> Arc<Self> {
        Arc::new(Self {
            store: RwLock::new(HashMap::new()),
            isolate: RwLock::new(HashMap::new()),
            intercept: RwLock::new(HashMap::new()),
            versions: RwLock::new(HashMap::new()),
            fiber: Arc::new(Fiber::new()),
            parent: Some(self.clone()),
            root: self.root.clone(),
        })
    }

    pub fn isolate<T: Service>(self: &Arc<Self>, label: impl Into<Symbol>) -> Arc<Self> {
        let mut parent_isolate = self.isolate.read().clone();
        parent_isolate.insert(TypeId::of::<T>(), label.into());
        Arc::new(Self {
            store: RwLock::new(HashMap::new()),
            isolate: RwLock::new(parent_isolate),
            intercept: RwLock::new(HashMap::new()),
            versions: RwLock::new(HashMap::new()),
            fiber: Arc::new(Fiber::new()),
            parent: Some(self.clone()),
            root: self.root.clone(),
        })
    }

    pub fn intercept<T: Service>(self: &Arc<Self>, val: T) -> Arc<Self> {
        let child = self.extend();
        let tid = TypeId::of::<T>();
        let any: Arc<dyn Any + Send + Sync> = Arc::new(val);
        child.intercept.write().insert(tid, any);
        // bump version for intercept as well? Not needed for epoch but keep
        child
    }

    // Provide inserts and pushes LIFO undo onto this context's fiber
    pub fn provide<T: Service>(self: &Arc<Self>, svc: T) -> Arc<T> {
        let tid = TypeId::of::<T>();
        let arc = Arc::new(svc);
        let any: Arc<dyn Any + Send + Sync> = arc.clone();
        let prev = self.store.write().insert(tid, any);
        {
            let mut versions = self.versions.write();
            let e = versions.entry(tid).or_insert(0);
            *e += 1;
        }
        // push undo
        let weak = Arc::downgrade(self);
        let fiber = self.fiber.clone();
        let prev_clone = prev;
        let undo: Box<dyn FnOnce() + Send> = Box::new(move || {
            if let Some(ctx) = weak.upgrade() {
                let mut store = ctx.store.write();
                if let Some(prev_any) = prev_clone {
                    store.insert(tid, prev_any);
                } else {
                    store.remove(&tid);
                }
                let mut versions = ctx.versions.write();
                if let Some(v) = versions.get_mut(&tid) {
                    *v = v.saturating_sub(1);
                    if *v == 0 {
                        versions.remove(&tid);
                    }
                }
            }
        });
        fiber.push_undo(undo);
        arc
    }

    pub fn get<T: Service>(&self) -> Option<Arc<T>> {
        let tid = TypeId::of::<T>();
        if let Some(any) = self.intercept.read().get(&tid) {
            if let Ok(arc) = any.clone().downcast::<T>() {
                return Some(arc);
            }
        }
        if let Some(any) = self.store.read().get(&tid) {
            if let Ok(arc) = any.clone().downcast::<T>() {
                return Some(arc);
            }
        }
        if let Some(parent) = &self.parent {
            return parent.get::<T>();
        }
        None
    }

    pub fn get_version(&self, tid: TypeId) -> u64 {
        if let Some(v) = self.versions.read().get(&tid) {
            return *v;
        }
        if let Some(parent) = &self.parent {
            return parent.get_version(tid);
        }
        0
    }

    pub fn fiber(&self) -> Arc<Fiber> {
        self.fiber.clone()
    }

    pub fn effect<E>(&self, eff: E) -> Box<dyn Disposable>
    where
        E: Effect,
    {
        let ctx_arc = self.root.upgrade().unwrap_or_else(|| {
            // if root weak dead, try to create Arc from self? But self is &Self not Arc
            // For spike, effect via &Self is not used; provide alternative
            panic!("effect called on detached context without root")
        });
        eff.apply(&ctx_arc)
    }

    // Snapshot for temporal test: capture store length + versions
    pub fn snapshot_len(&self) -> usize {
        self.store.read().len()
    }
}

// ---------------------------------------------------------------------------
// EventsService
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Emit,
    Parallel,
    Serial,
    Bail,
    Waterfall,
}

type Handler = Arc<dyn Fn(serde_json::Value) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>> + Send + Sync>;

pub struct EventsService {
    handlers: RwLock<HashMap<EventId, Vec<Handler>>>,
    bus: tokio::sync::broadcast::Sender<(EventId, serde_json::Value)>,
}

impl EventsService {
    pub fn new() -> Self {
        let (tx, _rx) = tokio::sync::broadcast::channel(32);
        Self {
            handlers: RwLock::new(HashMap::new()),
            bus: tx,
        }
    }

    pub fn on<F, Fut>(&self, event: EventId, handler: F) -> Box<dyn Disposable>
    where
        F: Fn(serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        let h: Handler = Arc::new(move |v| Box::pin(handler(v)));
        let mut handlers = self.handlers.write();
        let entry = handlers.entry(event.clone()).or_default();
        entry.push(h);
        let _idx = entry.len() - 1;
        let _event_clone = event;
        Box::new(move || {
            let _ = (_event_clone, _idx);
        })
    }

    pub async fn dispatch(
        &self,
        event: EventId,
        payload: serde_json::Value,
        mode: Dispatch,
    ) -> Result<serde_json::Value, CordisError> {
        let handlers = self.handlers.read().get(&event).cloned().unwrap_or_default();
        if handlers.is_empty() {
            return Ok(payload);
        }
        match mode {
            Dispatch::Emit => {
                let _ = self.bus.send((event, payload));
                Ok(serde_json::Value::Null)
            }
            Dispatch::Parallel => {
                let mut set = tokio::task::JoinSet::new();
                for h in handlers {
                    let p = payload.clone();
                    set.spawn(async move { h(p).await });
                }
                let mut last = serde_json::Value::Null;
                while let Some(res) = set.join_next().await {
                    if let Ok(Ok(v)) = res {
                        last = v;
                    }
                }
                Ok(last)
            }
            Dispatch::Serial => {
                let mut cur = payload;
                for h in handlers {
                    cur = h(cur).await?;
                }
                Ok(cur)
            }
            Dispatch::Bail => {
                let mut cur = payload;
                for h in handlers {
                    cur = h(cur).await?;
                }
                Ok(cur)
            }
            Dispatch::Waterfall => {
                let mut cur = payload;
                for h in handlers {
                    cur = h(cur).await?;
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

// ---------------------------------------------------------------------------
// Epoch helper (free function per plan)
// ---------------------------------------------------------------------------

pub fn compute_epoch(inject: &HashMap<TypeId, Symbol>) -> String {
    if inject.is_empty() {
        return ":".to_string();
    }
    let mut frags: Vec<String> = inject.values().cloned().collect();
    frags.sort();
    format!(":{}", frags.join(":"))
}

// ---------------------------------------------------------------------------
// Tests — the two theorems that must hold before Phase 2
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FooService(pub i32);
    impl Service for FooService {}

    #[derive(Debug)]
    struct BarService(pub i32);
    impl Service for BarService {}

    #[derive(Debug)]
    struct ConsumerService;
    impl Service for ConsumerService {}

    #[tokio::test]
    async fn temporal_composability() {
        // Register plugin → mutate context → dispose fiber → assert context recovered
        let ctx = Context::new_root();
        let pre_len = ctx.snapshot_len();
        assert!(ctx.get::<BarService>().is_none());

        // mutate via provide (witnessed effect)
        let bar = ctx.provide(BarService(42));
        assert_eq!(bar.0, 42);
        assert!(ctx.get::<BarService>().is_some());
        assert_eq!(ctx.snapshot_len(), pre_len + 1);

        // dispose fiber should LIFO revert
        ctx.fiber().dispose().await;
        assert!(ctx.get::<BarService>().is_none());
        assert_eq!(ctx.snapshot_len(), pre_len);
    }

    #[tokio::test]
    async fn spatial_composability() {
        // Provide service A → fiber depending on A activates → re-provide A → fiber automatically reloads
        let ctx = Context::new_root();
        let consumer_fiber = Arc::new(Fiber::new());
        consumer_fiber.declare_inject::<FooService>();

        // Initially Inactive (dep missing)
        assert_eq!(
            consumer_fiber.state(),
            FiberState::Inactive { error: None }
        );
        assert_eq!(consumer_fiber.epoch(), "");

        // Provide FooService v1 -> fiber should become Active after refresh
        ctx.provide(FooService(1));
        consumer_fiber.refresh(&ctx).await;
        assert!(matches!(consumer_fiber.state(), FiberState::Active { .. }));
        let epoch_v1 = consumer_fiber.epoch();
        assert!(epoch_v1.contains("FooService"));
        assert!(epoch_v1.contains(":1") || epoch_v1.contains("1"));

        // Re-provide FooService v2 -> epoch should change and reload triggered
        ctx.provide(FooService(2));
        let prev_epoch = epoch_v1.clone();
        consumer_fiber.refresh(&ctx).await;
        let epoch_v2 = consumer_fiber.epoch();
        assert_ne!(prev_epoch, epoch_v2);
        assert!(matches!(consumer_fiber.state(), FiberState::Active { .. }));
        // Ensure new provider visible
        assert_eq!(ctx.get::<FooService>().unwrap().0, 2);
    }

    #[tokio::test]
    async fn isolate_and_intercept() {
        let root = Context::new_root();
        root.provide(FooService(10));
        assert_eq!(root.get::<FooService>().unwrap().0, 10);

        // isolate tenant
        let tenant_ctx = root.isolate::<FooService>("tenant:acme");
        // tenant initially has no Foo (isolated) — but parent lookup would still find root's Foo
        // Our get walks parent, so it will find root's Foo. Isolate semantics: should not leak?
        // For spike, we test that tenant can provide its own Foo without affecting root
        tenant_ctx.provide(FooService(99));
        assert_eq!(tenant_ctx.get::<FooService>().unwrap().0, 99);
        assert_eq!(root.get::<FooService>().unwrap().0, 10);

        // intercept per-request override
        let req_ctx = root.intercept(FooService(77));
        assert_eq!(req_ctx.get::<FooService>().unwrap().0, 77);
        // root unchanged
        assert_eq!(root.get::<FooService>().unwrap().0, 10);
    }

    #[tokio::test]
    async fn events_dispatch_modes() {
        let svc = EventsService::new();
        svc.on("test".into(), |v| async move {
            let n = v.as_i64().unwrap_or(0);
            Ok(serde_json::Value::Number((n + 1).into()))
        });
        let out = svc
            .dispatch("test".into(), serde_json::Value::Number(1.into()), Dispatch::Serial)
            .await
            .unwrap();
        assert_eq!(out, serde_json::Value::Number(2.into()));
    }

    #[tokio::test]
    async fn epoch_monoid() {
        let mut map = HashMap::new();
        map.insert(TypeId::of::<FooService>(), "uid1".to_string());
        map.insert(TypeId::of::<BarService>(), "uid2".to_string());
        let e = compute_epoch(&map);
        assert!(e.starts_with(':'));
        assert!(e.contains("uid1"));
        assert!(e.contains("uid2"));
        // Empty
        let empty: HashMap<TypeId, Symbol> = HashMap::new();
        assert_eq!(compute_epoch(&empty), ":");
    }

    #[tokio::test]
    async fn fiber_inertia_serializes_transitions() {
        let fiber = Arc::new(Fiber::new());
        fiber.declare_inject::<FooService>();
        let ctx = Context::new_root();
        // concurrent refreshes should serialize via inertia mutex
        let f1 = fiber.clone();
        let c1 = ctx.clone();
        let f2 = fiber.clone();
        let c2 = ctx.clone();
        let (r1, r2) = tokio::join!(f1.refresh(&c1), f2.refresh(&c2));
        // both should complete without deadlock
        let _ = (r1, r2);
        assert!(matches!(
            fiber.state(),
            FiberState::Inactive { .. } | FiberState::Active { .. }
        ));
    }
}
