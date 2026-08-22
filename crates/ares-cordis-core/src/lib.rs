#![allow(missing_docs)]
#![allow(dead_code)]

use parking_lot::{Mutex, RwLock};
use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::watch;

static NEXT_FIBER_ID: AtomicUsize = AtomicUsize::new(1);

// Inventory / linkme static registration — real compile-time collection
#[cfg(feature = "inventory")]
pub struct CordisInventory {
    pub name: &'static str,
}

#[cfg(feature = "inventory")]
inventory::collect!(CordisInventory);

#[cfg(feature = "inventory")]
inventory::submit! {
    CordisInventory { name: "RegistryService" }
}

#[cfg(feature = "inventory")]
inventory::submit! {
    CordisInventory { name: "EventsService" }
}

#[cfg(feature = "inventory")]
inventory::submit! {
    CordisInventory { name: "ReflectService" }
}

#[cfg(feature = "inventory")]
inventory::submit! {
    CordisInventory { name: "Loader" }
}

#[cfg(feature = "inventory")]
inventory::submit! {
    CordisInventory { name: "SchedulerService" }
}

#[cfg(feature = "inventory")]
inventory::submit! {
    CordisInventory { name: "AgentExecutionService" }
}

#[cfg(feature = "inventory")]
inventory::submit! {
    CordisInventory { name: "UnifiedToolService" }
}

#[cfg(feature = "inventory")]
inventory::submit! {
    CordisInventory { name: "LlmService" }
}

#[cfg(feature = "inventory")]
pub fn inventory_len() -> usize {
    inventory::iter::<CordisInventory>.into_iter().count()
}

#[cfg(not(feature = "inventory"))]
pub fn inventory_len() -> usize {
    0
}

use thiserror::Error;

pub mod loader;
pub use loader::{Entry, EntryTree, Loader};

pub mod watcher;
pub mod hmr;
pub mod registry;
pub use registry::{Plugin, RegistryService};

#[cfg(feature = "rhai")]
pub mod rhai_service;
#[cfg(feature = "rhai")]
pub use rhai_service::{RhaiPlugin, RhaiService, RhaiServiceConfig};

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
    fn apply(&self, ctx: &Context) -> Box<dyn Disposable>;
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

pub type ServiceInitFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Box<dyn Disposable>>, CordisError>> + Send + 'a>>;

// ---------------------------------------------------------------------------
// Service trait
// ---------------------------------------------------------------------------

pub trait Service: Send + Sync + 'static {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn init(&self, _ctx: &Arc<Context>) -> ServiceInitFuture<'_> {
        Box::pin(async move { Ok(None) })
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
    /// A plugin activation is in flight (Cordis `LOADING`).
    Loading,
    /// Activation finished with an error; the plugin is not serving (Cordis `FAILED`).
    Failed { error: Option<String> },
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

    /// Record a lifecycle state transition (used by the registry while a plugin
    /// is loading or after it fails). Kept `pub(crate)` so sibling modules can
    /// drive the state machine without exposing write access publicly.
    pub(crate) fn set_state(&self, state: FiberState) {
        *self.state.write() = state;
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
        let prev = self.state.read().clone();
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
            *self.state.write() = FiberState::Active { epoch: new_epoch.clone() };
            if prev != *self.state.read() {
                tracing::info!(from=?prev, to=?*self.state.read(), epoch=%new_epoch, "Cordis fiber transition");
            }
        } else {
            *self.state.write() = FiberState::Inactive { error: None };
            if prev != *self.state.write() {
                tracing::info!(from=?prev, to=?*self.state.write(), epoch=%new_epoch, "Cordis fiber transition");
            }
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

    /// Apply a config change to a live fiber by recomputing its epoch and
    /// re-running the dependency-satisfaction check against `ctx`.  This is the
    /// synchronization point the Loader calls from its `UpdateConfig` arm; it
    /// is additive and non-destructive (unlike `dispose`), so a config-only
    /// change keeps the fiber's accumulator and committed view intact.
    pub async fn update(&self, ctx: &Arc<Context>) {
        self.refresh(ctx).await;
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
        // Notify dependents that T was provided/updated (reactive cascade)
        if let Some(reflect) = self.get::<ReflectService>() {
            reflect.notify(tid);
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

    /// Remove a service from the store and trigger deactivation cascade.
    /// Pushes the inverse (re-provide) onto the fiber's accumulator for LIFO reversal.
    pub fn remove<T: Service>(self: &Arc<Self>) -> Option<Arc<T>> {
        let tid = TypeId::of::<T>();
        let removed = {
            let mut store = self.store.write();
            store.remove(&tid)
        };
        if let Some(any) = removed {
            // Adjust version down (or remove entirely)
            {
                let mut versions = self.versions.write();
                if let Some(v) = versions.get_mut(&tid) {
                    *v = v.saturating_sub(1);
                    if *v == 0 {
                        versions.remove(&tid);
                    }
                }
            }
            // Notify dependents to deactivate
            if let Some(reflect) = self.get::<ReflectService>() {
                reflect.notify(tid);
            }
            // Push undo (re-provide) for LIFO reversal
            let weak = Arc::downgrade(self);
            let fiber = self.fiber.clone();
            let any_clone = any.clone();
            let undo: Box<dyn FnOnce() + Send> = Box::new(move || {
                if let Some(ctx) = weak.upgrade() {
                    ctx.store.write().insert(tid, any_clone);
                    let mut versions = ctx.versions.write();
                    let e = versions.entry(tid).or_insert(0);
                    *e += 1;
                }
            });
            fiber.push_undo(undo);
            // Downcast to the concrete type
            any.downcast::<T>().ok()
        } else {
            None
        }
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

    pub fn isolate_label(&self, tid: TypeId) -> Option<Symbol> {
        if let Some(label) = self.isolate.read().get(&tid).cloned() {
            return Some(label);
        }
        if let Some(parent) = &self.parent {
            return parent.isolate_label(tid);
        }
        None
    }

    /// TypeIds currently provided in this context's store (not parent/intercept).
    pub fn provided_type_ids(&self) -> Vec<TypeId> {
        self.store.read().keys().copied().collect()
    }

    /// Record an isolate namespace on this context without forking a child.
    ///
    /// Loader uses this after a factory `provide`s so `get_isolated` can find
    /// the new service under `Entry.isolate` while `get` still works on boot.
    pub fn bind_isolate(&self, tid: TypeId, label: impl Into<Symbol>) {
        self.isolate.write().insert(tid, label.into());
    }

    /// Record an intercept override on this context without forking a child.
    pub fn bind_intercept<T: Service>(&self, val: T) {
        let tid = TypeId::of::<T>();
        let any: Arc<dyn Any + Send + Sync> = Arc::new(val);
        self.intercept.write().insert(tid, any);
    }

    /// Retrieve a service only if it was provided in a context whose isolate
    /// namespace for `T` matches `label`. Walks the context chain but skips
    /// any frame whose isolate label for `T` differs from the requested one.
    pub fn get_isolated<T: Service>(&self, label: &str) -> Option<Arc<T>> {
        let tid = TypeId::of::<T>();
        let my_label = self.isolate.read().get(&tid).cloned();
        match my_label.as_deref() {
            Some(l) if l == label => {
                // Matching namespace — check this context's store
                if let Some(any) = self.store.read().get(&tid) {
                    if let Ok(arc) = any.clone().downcast::<T>() {
                        return Some(arc);
                    }
                }
                // Continue up the chain
                if let Some(parent) = &self.parent {
                    return parent.get_isolated::<T>(label);
                }
                None
            }
            Some(_) => {
                // Different namespace — do not look here or in parent
                None
            }
            None => {
                // No isolate entry for T in this frame — skip to parent
                if let Some(parent) = &self.parent {
                    return parent.get_isolated::<T>(label);
                }
                None
            }
        }
    }

    /// Create a child context where `get::<T>()` returns `val` as an override.
    /// Alias for `intercept` — explicitly named for per-request model pinning.
    pub fn with_intercept<T: Service>(self: &Arc<Self>, val: T) -> Arc<Self> {
        self.intercept(val)
    }

    /// Wait until `T` is provided on this context (or a parent). Returns the service.
    /// Polls with a short sleep so a concurrent `provide` unblocks this call.
    pub async fn inject<T: Service>(self: &Arc<Self>) -> Arc<T> {
        loop {
            if let Some(value) = self.get::<T>() {
                return value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    pub fn provide_arc<T: Service>(self: &Arc<Self>, svc: Arc<T>) -> Arc<T> {
        let tid = TypeId::of::<T>();
        let any: Arc<dyn Any + Send + Sync> = svc.clone();
        let prev = self.store.write().insert(tid, any);
        {
            let mut versions = self.versions.write();
            let e = versions.entry(tid).or_insert(0);
            *e += 1;
        }
        // Notify dependents that T was provided/updated (reactive cascade)
        if let Some(reflect) = self.get::<ReflectService>() {
            reflect.notify(tid);
        }
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
        svc
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

    pub async fn plugin<S: Service>(self: &Arc<Self>, svc: S) -> Result<FiberId, CordisError> {
        let tid = TypeId::of::<S>();
        if self.store.read().contains_key(&tid) {
            return Err(CordisError::Configuration(format!(
                "duplicate provider for {:?}",
                tid
            )));
        }
        let fiber = self.fiber.clone();
        *fiber.state.write() = FiberState::Loading;
        let disposable = match svc.init(self).await {
            Ok(d) => d,
            Err(e) => {
                *fiber.state.write() = FiberState::Failed {
                    error: Some(e.to_string()),
                };
                return Err(e);
            }
        };
        let svc_arc = self.provide(svc);
        let fid = NEXT_FIBER_ID.fetch_add(1, Ordering::SeqCst) as u64;
        if let Some(d) = disposable {
            let undo: Box<dyn FnOnce() + Send> = Box::new(move || {
                d.dispose();
            });
            fiber.push_undo(undo);
        }
        if svc_arc.check() {
            let epoch = fiber.compute_epoch(self);
            *fiber.epoch.write() = epoch.clone();
            *fiber.state.write() = FiberState::Active { epoch };
        } else {
            *fiber.state.write() = FiberState::Inactive { error: None };
            if let Some(reflect) = self.get::<ReflectService>() {
                reflect.notify(tid);
            }
        }
        if let Some(reflect) = self.get::<ReflectService>() {
            let _ = reflect.ensure_notifier(tid);
            reflect.set_context(self);
        }
        Ok(fid)
    }

    pub async fn plugin_with<P: Plugin>(self: &Arc<Self>, plugin: P, config: P::Config) -> Result<FiberId, CordisError> {
        if let Some(registry) = self.get::<RegistryService>() {
            return registry.plugin(self, plugin, config);
        }
        let tid = TypeId::of::<P::Provides>();
        if self.store.read().contains_key(&tid) {
            return Err(CordisError::Configuration(format!(
                "duplicate provider for {:?}",
                tid
            )));
        }
        *self.fiber.state.write() = FiberState::Loading;
        let provides = match plugin.apply(self, config) {
            Ok(p) => p,
            Err(e) => {
                *self.fiber.state.write() = FiberState::Failed {
                    error: Some(e.to_string()),
                };
                return Err(e);
            }
        };
        self.provide_arc(provides);
        *self.fiber.state.write() = FiberState::Active {
            epoch: self.fiber.compute_epoch(self),
        };
        let fid = NEXT_FIBER_ID.fetch_add(1, Ordering::SeqCst) as u64;
        Ok(fid)
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

/// The `next` continuation handed to a [`WaterfallHandler`].  It advances to the
/// next registered waterfall handler, or returns the passed payload unchanged once
/// the chain is exhausted.  It is `FnOnce`: a handler may call `next` at most once,
/// mirroring Cordis `next()` semantics.
type WaterfallNext =
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

pub struct EventsService {
    handlers: RwLock<HashMap<EventId, Vec<Handler>>>,
    waterfall_handlers: RwLock<HashMap<EventId, Vec<WaterfallHandler>>>,
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

    /// Register a Cordis `waterfall` around-middleware handler.
    ///
    /// `handler` receives the current payload and a `next` continuation.  Calling
    /// `next(payload)` runs the downstream chain and yields its (possibly
    /// transformed) result; NOT calling `next` short-circuits the chain so later
    /// handlers do not run.  Handlers registered here are only invoked by
    /// [`dispatch`](EventsService::dispatch) with [`Dispatch::Waterfall`]; the plain
    /// [`on`](EventsService::on) registry is used for emit/parallel/serial/bail.
    pub fn on_waterfall<F, Fut>(
        &self,
        event: EventId,
        handler: F,
    ) -> Box<dyn Disposable>
    where
        F: Fn(serde_json::Value, WaterfallNext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<serde_json::Value, CordisError>> + Send + 'static,
    {
        let h: WaterfallHandler = Arc::new(move |v, next| Box::pin(handler(v, next)));
        let mut handlers = self.waterfall_handlers.write();
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
        match mode {
            // Cordis `waterfall` uses its own around-middleware registry, so it
            // must not be short-circuited by the plain-handler emptiness check.
            Dispatch::Waterfall => {
                let wf_handlers = self
                    .waterfall_handlers
                    .read()
                    .get(&event)
                    .cloned()
                    .unwrap_or_default();
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
            Box::pin(
                async move { run_waterfall_chain(remaining, index + 1, p).await },
            ) as Pin<
                Box<dyn Future<Output = Result<serde_json::Value, CordisError>> + Send>,
            >
        };
        handler(payload, Box::new(next)).await
    })
}

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

// RegistryService and Plugin live in registry.rs to keep single-source discipline
// and isolate-aware checks in one place. Re-exported here for ergonomics.

// ---------------------------------------------------------------------------
// PluginRegistry — name → factory map consumed by `Loader::instantiate`
// ---------------------------------------------------------------------------

/// Factory closure that turns one declarative entry into a live fiber.
///
/// The body must call [`Context::plugin`] (directly or via a helper) so that
/// single-source discipline applies: a factory whose service is already
/// provided fails with `CordisError::Configuration("duplicate provider …")`
/// instead of silently shadowing it.
pub type PluginFactory =
    Arc<dyn Fn(&Arc<Context>, &serde_json::Value) -> Result<FiberId, CordisError> + Send + Sync>;

/// Name-keyed directory of [`PluginFactory`] closures.
///
/// Registered at bootstrap (`root_ctx.provide(PluginRegistry::new())` +
/// `register(name, …)`); consulted by `Loader::instantiate` when applying
/// entries from `config/cordis-entries.toml`. Entries naming a plugin with no
/// registered factory fail their own instantiation but never abort startup.
pub struct PluginRegistry {
    factories: RwLock<HashMap<String, PluginFactory>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, name: &str, f: PluginFactory) {
        self.factories.write().insert(name.to_string(), f);
    }

    pub fn get(&self, name: &str) -> Option<PluginFactory> {
        self.factories.read().get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.factories.read().keys().cloned().collect()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Service for PluginRegistry {}

// ---------------------------------------------------------------------------
// ReflectService — Phase 3 unified hot-reload (watch + BFS via Fiber::refresh)
// ---------------------------------------------------------------------------

/// Unified hot-reload coordinator — replaces 60s `ArcSwap` polling.
///
/// Tracks `notifiers: RwLock<HashMap<TypeId, watch::Sender<()>>>` for DB/file
/// change fan-out and `dependents: RwLock<HashMap<TypeId, Vec<FiberId>>>` for
/// BFS dependency walks. `notify(TypeId)` BFS-walks `dependents` and calls
/// `Fiber::refresh` on each dependent fiber, using the same `Fiber` impl that
/// recomputes `epoch` from `inject` versions (see `Fiber::refresh`). Watch
/// channels are created lazily on `provide` via `ensure_notifier` — prove by
/// calling it on registry creation (e.g. `RuntimeToolRegistry` / `ProviderRegistry`
/// insertion). See `docs/cordis-mapping.md` §7, §11.
///
/// `fibers` / `fiber_provides` / `ctx` are extra bookkeeping for BFS + async
/// `refresh`; `notifiers` + `dependents` are the required fields per spec.
#[allow(dead_code)]
pub struct ReflectService {
    notifiers: RwLock<HashMap<TypeId, watch::Sender<()>>>,
    dependents: RwLock<HashMap<TypeId, Vec<FiberId>>>,
    fibers: RwLock<HashMap<FiberId, Arc<Fiber>>>,
    fiber_provides: RwLock<HashMap<FiberId, TypeId>>,
    ctx: RwLock<Option<Weak<Context>>>,
}

impl ReflectService {
    pub fn new() -> Self {
        Self {
            notifiers: RwLock::new(HashMap::new()),
            dependents: RwLock::new(HashMap::new()),
            fibers: RwLock::new(HashMap::new()),
            fiber_provides: RwLock::new(HashMap::new()),
            ctx: RwLock::new(None),
        }
    }

    /// Ensure a `watch` channel exists for `tid`; create lazily on `provide`.
    /// Returns a `Receiver` that callers can `changed().await` on for DB/file updates.
    /// This is the “provide watch channel creation on provide” hook — call after
    /// `ctx.provide::<T>(svc)` to prove compile-time insertion.
    pub fn ensure_notifier(&self, tid: TypeId) -> watch::Receiver<()> {
        let mut notifiers = self.notifiers.write();
        if let Some(sender) = notifiers.get(&tid) {
            return sender.subscribe();
        }
        let (tx, rx) = watch::channel(());
        notifiers.insert(tid, tx);
        rx
    }

    /// Convenience: ensure notifier for a `Service` type.
    pub fn ensure_notifier_for<T: Service>(&self) -> watch::Receiver<()> {
        self.ensure_notifier(TypeId::of::<T>())
    }

    /// Register that `fid` depends on `tid` (i.e. `fid.injects` contains `tid`).
    /// Populates `dependents` for BFS walks.
    pub fn register_dependent(&self, tid: TypeId, fid: FiberId) {
        let mut deps = self.dependents.write();
        let entry = deps.entry(tid).or_default();
        if !entry.contains(&fid) {
            entry.push(fid);
        }
    }

    /// Register a fiber and what `TypeId` it provides (for transitive BFS).
    /// Call from `RegistryService::plugin` after allocating `fid`.
    pub fn register_fiber(&self, fid: FiberId, fiber: Arc<Fiber>, provides: TypeId) {
        self.fibers.write().insert(fid, fiber);
        self.fiber_provides.write().insert(fid, provides);
    }

    /// Remember the root `Context` weakly so `notify` can `upgrade()` and call
    /// `Fiber::refresh` without caller passing `ctx`.
    pub fn set_context(&self, ctx: &Arc<Context>) {
        *self.ctx.write() = Some(Arc::downgrade(ctx));
    }

    /// BFS walks `dependents` starting at `tid`, notifies `watch` senders,
    /// and spawns `Fiber::refresh` for each dependent fiber (uses existing
    /// `Fiber::refresh` impl). This replaces the 60s `ArcSwap` poll;
    /// registry reload is now triggered by `notify` via `watch` channel on DB
    /// `NOTIFY`/`LISTEN` or file change, not a timer.
    pub fn notify(&self, tid: TypeId) {
        // Snapshot context weakly; if no context, still notify watch channels
        let ctx_opt = self.ctx.read().as_ref().and_then(|w| w.upgrade());

        // Emit service.changed event via EventsService (fire-and-forget)
        if let Some(ctx) = &ctx_opt {
            if let Some(events) = ctx.get::<EventsService>() {
                let payload = serde_json::json!({
                    "type_id": format!("{:?}", tid),
                    "event": "service.changed"
                });
                tokio::spawn(async move {
                    let _ = events.dispatch("service.changed".into(), payload, Dispatch::Emit).await;
                });
            }
        }

        let mut queue = VecDeque::new();
        let mut visited_type = HashSet::new();
        let mut visited_fiber = HashSet::new();
        queue.push_back(tid);
        visited_type.insert(tid);
        while let Some(cur) = queue.pop_front() {
            // Fan-out via watch channel
            if let Some(sender) = self.notifiers.read().get(&cur).cloned() {
                let _ = sender.send(());
            }
            // BFS over dependent fibers
            let fids = self
                .dependents
                .read()
                .get(&cur)
                .cloned()
                .unwrap_or_default();
            for fid in fids {
                if !visited_fiber.insert(fid) {
                    continue;
                }
                let fiber_opt = self.fibers.read().get(&fid).cloned();
                if let Some(fiber) = fiber_opt {
                    if let Some(ctx) = ctx_opt.clone() {
                        let fiber_clone = fiber.clone();
                        tokio::spawn(async move {
                            fiber_clone.refresh(&ctx).await;
                        });
                    }
                    // Transitive: if this fiber provides a TypeId, enqueue its dependents
                    if let Some(provided) = self.fiber_provides.read().get(&fid).copied() {
                        if visited_type.insert(provided) {
                            queue.push_back(provided);
                        }
                    }
                }
            }
        }
    }

    /// Async variant that `await`s each `Fiber::refresh` directly (for tests / direct callers that have `ctx`).
    #[allow(clippy::await_holding_lock)]
    pub async fn notify_with_ctx(&self, tid: TypeId, ctx: &Arc<Context>) {
        let mut queue = VecDeque::new();
        let mut visited_type = HashSet::new();
        let mut visited_fiber = HashSet::new();
        queue.push_back(tid);
        visited_type.insert(tid);
        while let Some(cur) = queue.pop_front() {
            if let Some(sender) = self.notifiers.read().get(&cur).cloned() {
                let _ = sender.send(());
            }
            let fids = self
                .dependents
                .read()
                .get(&cur)
                .cloned()
                .unwrap_or_default();
            for fid in fids {
                if !visited_fiber.insert(fid) {
                    continue;
                }
                let fiber = { self.fibers.read().get(&fid).cloned() };
                if let Some(fiber) = fiber {
                    fiber.refresh(ctx).await;
                    if let Some(provided) = self.fiber_provides.read().get(&fid).copied() {
                        if visited_type.insert(provided) {
                            queue.push_back(provided);
                        }
                    }
                }
            }
        }
    }

    /// Get a `watch::Receiver` if already created (no creation).
    pub fn subscribe(&self, tid: TypeId) -> Option<watch::Receiver<()>> {
        self.notifiers.read().get(&tid).map(|s| s.subscribe())
    }
}

impl Default for ReflectService {
    fn default() -> Self {
        Self::new()
    }
}

impl Service for ReflectService {}

// Inventory/linkme static registration placeholder (preferred for production).
// Real static registration would use `inventory::submit!` or `linkme::distributed_slice`
// to collect `fn(&Arc<Context>) -> Result<FiberId, CordisError>` at compile time.
// This spike stubs it — Phase 3 Loader will drive declarative reconciliation.
// Behind `#[cfg(feature = "hmr")]`, `libloading` would `dlopen` a `.so` and call `Plugin::apply`
// via an `extern "C"` entry point; if ABI fragility blocks, fallback is file-watch + full fiber reload
// (see docs/cordis-mapping.md §11 — 90% value without dynamic code).

// ---------------------------------------------------------------------------
// LoaderJournal — live bookkeeping for `Loader::execute_action` / `instantiate`
// ---------------------------------------------------------------------------

/// One record in the [`LoaderJournal`]: the plugin label owning an entry, the
/// last applied config, the live fiber id when known, and a monotonically
/// increasing generation counter.  `generation` lets reconciliation callers
/// detect whether an entry's config actually changed (see
/// [`Loader::execute_action`](crate::loader::Loader::execute_action)).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JournalRecord {
    pub plugin: String,
    pub config: serde_json::Value,
    pub fiber_id: Option<FiberId>,
    pub generation: u64,
}

/// Optional journal that makes the loader lifecycle real for `UpdateConfig`
/// and `Retire` arms.
///
/// Provide it as a `Service` (`ctx.provide(LoaderJournal::new())`) so
/// [`Context::get::<LoaderJournal>`] returns the shared handle; when absent,
/// [`Loader::execute_action`](crate::loader::Loader::execute_action) and
/// [`Loader::instantiate`](crate::loader::Loader::instantiate) degrade to
/// log-only.  Every mutation bumps `generation`; the journal is the single
/// source of truth for "is this entry live, with which fiber, at what
/// config/version".
#[derive(Clone, Default)]
pub struct LoaderJournal {
    records: Arc<RwLock<HashMap<String, JournalRecord>>>,
}

impl LoaderJournal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a record, bumping generation by 1 from the prior value
    /// (or from 0 for a fresh id).
    pub fn upsert(
        &self,
        id: &str,
        plugin: &str,
        config: serde_json::Value,
        fiber_id: Option<FiberId>,
    ) {
        let mut records = self.records.write();
        let generation = records.get(id).map(|r| r.generation).unwrap_or(0) + 1;
        records.insert(
            id.to_string(),
            JournalRecord {
                plugin: plugin.to_string(),
                config,
                fiber_id,
                generation,
            },
        );
    }

    /// Replace the stored config for `id`, bumping generation, and optionally
    /// refresh the tracked fiber id.  Returns the prior record if present.
    pub fn update_config(
        &self,
        id: &str,
        new_config: serde_json::Value,
        fiber_id: Option<FiberId>,
    ) -> Option<JournalRecord> {
        let mut records = self.records.write();
        let record = records.get_mut(id)?;
        record.config = new_config;
        if let Some(fid) = fiber_id {
            record.fiber_id = Some(fid);
        }
        record.generation += 1;
        Some(record.clone())
    }

    /// Remove `id` from the journal (retirement).  Returns the removed record.
    pub fn retire(&self, id: &str) -> Option<JournalRecord> {
        self.records.write().remove(id)
    }

    pub fn get(&self, id: &str) -> Option<JournalRecord> {
        self.records.read().get(id).cloned()
    }

    pub fn len(&self) -> usize {
        self.records.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.read().is_empty()
    }
}

impl Service for LoaderJournal {}

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

    #[tokio::test]
    async fn registry_single_source_discipline() {
        let ctx = Context::new_root();
        let registry = RegistryService::new();

        struct FooPlugin;
        impl Plugin for FooPlugin {
            type Config = ();
            type Provides = FooService;
            fn apply(
                &self,
                _ctx: &Arc<Context>,
                _cfg: Self::Config,
            ) -> Result<Arc<Self::Provides>, CordisError> {
                Ok(Arc::new(FooService(1)))
            }
        }

        struct FooPlugin2;
        impl Plugin for FooPlugin2 {
            type Config = ();
            type Provides = FooService;
            fn apply(
                &self,
                _ctx: &Arc<Context>,
                _cfg: Self::Config,
            ) -> Result<Arc<Self::Provides>, CordisError> {
                Ok(Arc::new(FooService(2)))
            }
        }

        let fid1 = registry.plugin(&ctx, FooPlugin, ()).expect("first plugin ok");
        assert!(registry.get_fiber(fid1).is_some());
        let err = registry.plugin(&ctx, FooPlugin2, ()).expect_err("duplicate should fail");
        assert!(err.to_string().contains("duplicate provider"));
        // original still present
        assert!(registry.get_fiber(fid1).is_some());
    }

    #[tokio::test]
    async fn test_event_bus_dispatch_received() {
        let ctx = Context::new_root();
        let events = ctx.provide(EventsService::new());

        // Register a listener
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = received.clone();
        events.on("test.event".into(), move |payload| {
            let r = received_clone.clone();
            async move {
                r.lock().push(payload.clone());
                Ok(payload)
            }
        });

        // Dispatch
        let payload = serde_json::json!({"key": "value"});
        events
            .dispatch("test.event".into(), payload.clone(), Dispatch::Serial)
            .await
            .unwrap();

        // Verify received
        let msgs = received.lock();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["key"], "value");
    }

    #[tokio::test]
    async fn test_reactive_activation_deactivation() {
        // Service A that fiber depends on
        struct DepService;
        impl Service for DepService {}

        // Create root context with ReflectService
        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        let reflect = ctx.get::<ReflectService>().unwrap();
        reflect.set_context(&ctx);

        // Create fiber that injects DepService
        let fiber = Arc::new(Fiber::new());
        fiber.declare_inject::<DepService>();
        let fid: FiberId = 100;
        reflect.register_dependent(TypeId::of::<DepService>(), fid);
        reflect.register_fiber(fid, fiber.clone(), TypeId::of::<DepService>());

        // Initially Inactive (dep not provided)
        fiber.refresh(&ctx).await;
        assert!(matches!(*fiber.state.read(), FiberState::Inactive { .. }));

        // Provide DepService -> fiber should activate via notify cascade
        ctx.provide(DepService);
        // Give tokio a chance to run the spawned refresh
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            matches!(*fiber.state.read(), FiberState::Active { .. }),
            "fiber should be Active after provide, got: {:?}",
            fiber.state()
        );

        // Remove DepService -> fiber should deactivate via notify cascade
        ctx.remove::<DepService>();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            matches!(*fiber.state.read(), FiberState::Inactive { .. }),
            "fiber should be Inactive after remove, got: {:?}",
            fiber.state()
        );
    }

    #[tokio::test]
    async fn test_isolate_disjoint_namespaces() {
        #[derive(Debug)]
        struct ToolSvc(String);
        impl Service for ToolSvc {}

        let root = Context::new_root();

        // Create two isolated contexts for different tenants
        let ctx_a = root.isolate::<ToolSvc>("tenant_a");
        ctx_a.provide(ToolSvc("tool_for_a".into()));

        let ctx_b = root.isolate::<ToolSvc>("tenant_b");
        ctx_b.provide(ToolSvc("tool_for_b".into()));

        // Each tenant sees only its own service via get_isolated
        let svc_a = ctx_a.get_isolated::<ToolSvc>("tenant_a");
        assert!(svc_a.is_some());
        assert_eq!(svc_a.unwrap().0, "tool_for_a");

        let svc_b = ctx_b.get_isolated::<ToolSvc>("tenant_b");
        assert!(svc_b.is_some());
        assert_eq!(svc_b.unwrap().0, "tool_for_b");

        // Cross-tenant access returns None
        assert!(ctx_a.get_isolated::<ToolSvc>("tenant_b").is_none());
        assert!(ctx_b.get_isolated::<ToolSvc>("tenant_a").is_none());

        // Root has no isolated service
        assert!(root.get_isolated::<ToolSvc>("tenant_a").is_none());
        assert!(root.get_isolated::<ToolSvc>("tenant_b").is_none());
    }

    #[test]
    fn bind_isolate_labels_provided_service_in_place() {
        #[derive(Debug)]
        struct ToolSvc(String);
        impl Service for ToolSvc {}

        let root = Context::new_root();
        root.provide(ToolSvc("fleet".into()));
        root.bind_isolate(TypeId::of::<ToolSvc>(), "tenant:acme");
        let got = root
            .get_isolated::<ToolSvc>("tenant:acme")
            .expect("in-place isolate");
        assert_eq!(got.0, "fleet");
        assert!(root.get::<ToolSvc>().is_some());
    }

    #[tokio::test]
    async fn test_intercept_overrides_get() {
        #[derive(Debug)]
        struct ModelSvc {
            model: String,
        }
        impl Service for ModelSvc {}

        let root = Context::new_root();
        root.provide(ModelSvc {
            model: "gpt-4".into(),
        });

        // Root returns the original
        assert_eq!(root.get::<ModelSvc>().unwrap().model, "gpt-4");

        // with_intercept creates a child context where get returns the override
        let req_ctx = root.with_intercept(ModelSvc {
            model: "gpt-4o-mini".into(),
        });
        assert_eq!(req_ctx.get::<ModelSvc>().unwrap().model, "gpt-4o-mini");

        // Root remains unaffected
        assert_eq!(root.get::<ModelSvc>().unwrap().model, "gpt-4");

        // Stacking intercepts: innermost wins
        let inner_ctx = req_ctx.intercept(ModelSvc {
            model: "o1-preview".into(),
        });
        assert_eq!(inner_ctx.get::<ModelSvc>().unwrap().model, "o1-preview");
        // Outer still sees its own override
        assert_eq!(req_ctx.get::<ModelSvc>().unwrap().model, "gpt-4o-mini");
    }

    #[tokio::test]
    async fn inject_returns_immediately_when_already_provided() {
        let ctx = Context::new_root();
        ctx.provide(FooService(1));
        let got = ctx.inject::<FooService>().await;
        assert_eq!(got.name(), FooService(1).name());
        assert_eq!(got.0, 1);
    }

    #[tokio::test]
    async fn inject_waits_until_service_is_provided() {
        let ctx = Context::new_root();
        let waiter = ctx.clone();
        let handle = tokio::spawn(async move { waiter.inject::<FooService>().await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        ctx.provide(FooService(42));
        let got = tokio::time::timeout(std::time::Duration::from_millis(200), handle)
            .await
            .expect("inject should complete within 200ms")
            .expect("inject task should not panic");
        assert_eq!(got.0, 42);
    }

    #[tokio::test]
    async fn test_production_style_reactive_cycle() {
        #[derive(Debug)]
        struct Probe;
        impl Service for Probe {}

        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        let reflect = ctx.get::<ReflectService>().unwrap();
        reflect.set_context(&ctx);

        let f = Arc::new(Fiber::new());
        f.declare_inject::<Probe>();
        reflect.register_dependent(TypeId::of::<Probe>(), 777);
        reflect.register_fiber(777, f.clone(), TypeId::of::<Probe>());

        // Initially Inactive: dep not yet provided
        f.refresh(&ctx).await;
        assert!(
            matches!(f.state(), FiberState::Inactive { .. }),
            "expected Inactive before provide, got {:?}",
            f.state()
        );

        // Provide -> notify_with_ctx (synchronous, no sleeps) drives activation
        let _probe = ctx.provide(Probe);
        reflect.notify_with_ctx(TypeId::of::<Probe>(), &ctx).await;
        assert!(
            matches!(f.state(), FiberState::Active { .. }),
            "expected Active after provide, got {:?}",
            f.state()
        );

        // Remove -> notify_with_ctx drives deactivation
        ctx.remove::<Probe>();
        reflect.notify_with_ctx(TypeId::of::<Probe>(), &ctx).await;
        assert!(
            matches!(f.state(), FiberState::Inactive { .. }),
            "expected Inactive after remove, got {:?}",
            f.state()
        );
    }

    // -----------------------------------------------------------------------
    // EventsService dispatch parity with Cordis TS semantics
    // -----------------------------------------------------------------------

    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn events_emit_fire_and_forget_and_broadcast() {
        let svc = EventsService::new();
        // Each handler signals completion via an mpsc channel after doing work.
        let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(16);
        let mut bus_rx = svc.bus.subscribe();

        for i in 0..3 {
            let tx = done_tx.clone();
            svc.on("emit.test".into(), move |payload| {
                let tx = tx.clone();
                async move {
                    // simulate async work so dispatch must NOT await us
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    let _ = tx.send(()).await;
                    Ok(serde_json::json!({ "handler": i, "seen": payload }))
                }
            });
        }

        let payload = serde_json::json!({ "n": 1 });
        let start = std::time::Instant::now();
        let out = svc
            .dispatch("emit.test".into(), payload.clone(), Dispatch::Emit)
            .await
            .unwrap();
        let dispatch_elapsed = start.elapsed();

        // Emit returns immediately (fire-and-forget) with Null — it does NOT
        // await handler completion.
        assert_eq!(out, serde_json::Value::Null);
        assert!(
            dispatch_elapsed < std::time::Duration::from_millis(20),
            "emit returned after {:?} — should return immediately",
            dispatch_elapsed
        );

        // The raw event+payload was broadcast on the bus.
        let (evt, bus_payload) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            bus_rx.recv(),
        )
        .await
        .expect("bus should broadcast")
        .expect("bus recv should be a value");
        assert_eq!(evt, "emit.test");
        assert_eq!(bus_payload, payload);

        // Even though emit is fire-and-forget, every handler must still run to
        // completion before the test asserts.
        for _ in 0..3 {
            tokio::time::timeout(std::time::Duration::from_secs(1), done_rx.recv())
                .await
                .expect("handlers should complete")
                .expect("handler completion signal");
        }
    }

    #[tokio::test]
    async fn events_emit_invokes_registered_handler_counter() {
        // Spec: a handler registered via `on()` actually RUNS on `Emit`. Prove it
        // with an `Arc<AtomicUsize>` counter that the handler increments, then poll
        // (sleep loop) until it is > 0.
        let svc = EventsService::new();
        let counter = Arc::new(AtomicUsize::new(0));

        let c = counter.clone();
        svc.on("emit.counter".into(), move |payload| {
            let c = c.clone();
            async move {
                // Simulate a little async work so the spawn completes on the runtime.
                let n = payload.as_i64().unwrap_or(0);
                for _ in 0..n {
                    tokio::task::yield_now().await;
                }
                c.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::Value::Null)
            }
        });

        let out = svc
            .dispatch("emit.counter".into(), serde_json::json!(5), Dispatch::Emit)
            .await
            .unwrap();
        assert_eq!(out, serde_json::Value::Null);

        // Poll until the spawned handler has actually run (fire-and-forget means we
        // cannot await it directly).
        for _ in 0..100 {
            if counter.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            counter.load(Ordering::SeqCst) > 0,
            "emit handler should have run and incremented the counter"
        );
    }

    #[tokio::test]
    async fn events_serial_threads_payload_in_order() {
        // Spec: run handlers in order threading the payload through. Assert order and
        // payload threading: each handler appends a tag and reports the payload it saw.
        let svc = EventsService::new();
        let order = Arc::new(Mutex::new(Vec::new()));

        for tag in ["a", "b", "c"] {
            let o = order.clone();
            let tag = tag.to_string();
            svc.on("serial.test".into(), move |payload| {
                let o = o.clone();
                let tag = tag.clone();
                async move {
                    // Record that we saw the previous payload and our tag.
                    let mut prev = payload.as_i64().unwrap_or(0);
                    o.lock().push(format!("{}:{}", tag, prev));
                    // Propagate the payload forward: new value = previous + 10.
                    prev += 10;
                    Ok(serde_json::Value::Number(prev.into()))
                }
            });
        }

        let out = svc
            .dispatch("serial.test".into(), serde_json::Value::Number(1.into()), Dispatch::Serial)
            .await
            .unwrap();
        assert_eq!(out, serde_json::Value::Number(31.into()));
        // Handlers ran in registration order and each saw the prior handler's output.
        let seen = order.lock().clone();
        assert_eq!(
            seen,
            vec!["a:1".to_string(), "b:11".to_string(), "c:21".to_string()]
        );
    }

    #[tokio::test]
    async fn events_bail_stops_at_first_non_null_and_skips_later_handlers() {
        let svc = EventsService::new();
        let ran = Arc::new(AtomicUsize::new(0));

        // Handler 1 returns Null → does not bail, chain continues.
        let h1 = ran.clone();
        svc.on("bail.test".into(), move |_payload| {
            let r = h1.clone();
            async move {
                r.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::Value::Null)
            }
        });
        // Handler 2 returns a non-null value → bails.
        let h2 = ran.clone();
        svc.on("bail.test".into(), move |_payload| {
            let r = h2.clone();
            async move {
                r.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({ "bailed": true }))
            }
        });
        // Handler 3 must NOT run.
        let h3 = ran.clone();
        svc.on("bail.test".into(), move |_payload| {
            let r = h3.clone();
            async move {
                r.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::Value::Null)
            }
        });

        let payload = serde_json::json!({ "n": 1 });
        let out = svc
            .dispatch("bail.test".into(), payload.clone(), Dispatch::Bail)
            .await
            .unwrap();
        assert_eq!(out, serde_json::json!({ "bailed": true }));
        // Only the first two handlers ran; handler 3 was skipped.
        assert_eq!(ran.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn events_waterfall_handler_calls_next_and_receives_downstream_result() {
        let svc = EventsService::new();
        // Chain: outer wraps inner. The inner handler runs first during `next`,
        // then the outer transforms the downstream result.
        svc.on_waterfall("wf.next".into(), |payload, next| {
            let next = next;
            async move {
                let downstream = next(payload).await?;
                // The outer transforms what came back from downstream.
                let mut obj = downstream.as_object().cloned().unwrap_or_default();
                obj.insert("outer".into(), serde_json::json!(true));
                Ok(serde_json::Value::Object(obj))
            }
        });
        svc.on_waterfall("wf.next".into(), |payload, _next| {
            async move {
                let mut obj = payload.as_object().cloned().unwrap_or_default();
                obj.insert("inner_seen".into(), serde_json::json!(payload.get("value")));
                Ok(serde_json::Value::Object(obj))
            }
        });

        let payload = serde_json::json!({ "value": 42 });
        let out = svc
            .dispatch("wf.next".into(), payload, Dispatch::Waterfall)
            .await
            .unwrap();
        let obj = out.as_object().expect("waterfall output should be an object");
        // Inner ran (during next) and outer wrapped its result.
        assert_eq!(obj["inner_seen"], serde_json::json!(42));
        assert_eq!(obj["outer"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn events_waterfall_handler_short_circuits_skips_later_handlers() {
        let svc = EventsService::new();
        let ran = Arc::new(AtomicUsize::new(0));

        // First handler short-circuits: does NOT call next.
        let h1 = ran.clone();
        svc.on_waterfall("wf.short".into(), move |_payload, _next| {
            let r = h1.clone();
            async move {
                r.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({ "owned": true }))
            }
        });
        // Later handler must NOT run.
        let h2 = ran.clone();
        svc.on_waterfall("wf.short".into(), move |payload, next| {
            let r = h2.clone();
            async move {
                r.fetch_add(1, Ordering::SeqCst);
                next(payload).await
            }
        });

        let payload = serde_json::json!({ "n": 1 });
        let out = svc
            .dispatch("wf.short".into(), payload, Dispatch::Waterfall)
            .await
            .unwrap();
        assert_eq!(out, serde_json::json!({ "owned": true }));
        // The later handler never ran.
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn events_waterfall_empty_chain_returns_payload_unchanged() {
        let svc = EventsService::new();
        let payload = serde_json::json!({ "n": 7 });
        let out = svc
            .dispatch("wf.empty".into(), payload.clone(), Dispatch::Waterfall)
            .await
            .unwrap();
        assert_eq!(out, payload);
    }

    #[tokio::test]
    async fn events_parallel_propagates_aggregate_error() {
        let svc = EventsService::new();
        svc.on("par.test".into(), |_payload| async move {
            Ok(serde_json::json!({ "ok": 1 }))
        });
        svc.on("par.test".into(), |_payload| async move {
            Err(CordisError::Fiber("boom".into()))
        });
        svc.on("par.test".into(), |_payload| async move {
            Ok(serde_json::json!({ "ok": 2 }))
        });

        let payload = serde_json::json!({ "n": 1 });
        let err = svc
            .dispatch("par.test".into(), payload, Dispatch::Parallel)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("boom"),
            "parallel should propagate the handler error, got: {err}"
        );
    }

    #[tokio::test]
    async fn events_parallel_returns_a_value_when_no_handler_errors() {
        let svc = EventsService::new();
        svc.on("par2.test".into(), |_payload| async move {
            Ok(serde_json::json!({ "h": 1 }))
        });
        svc.on("par2.test".into(), |_payload| async move {
            Ok(serde_json::json!({ "h": 2 }))
        });

        let out = svc
            .dispatch("par2.test".into(), serde_json::json!({}), Dispatch::Parallel)
            .await
            .unwrap();
        // Execution order is non-deterministic; just require a non-error Ok value.
        assert!(out == serde_json::json!({ "h": 1 }) || out == serde_json::json!({ "h": 2 }));
    }
}
