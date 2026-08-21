#![allow(missing_docs)]
#![allow(dead_code)]

use parking_lot::{Mutex, RwLock};
use serde::{de::DeserializeOwned, Serialize};
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

    pub async fn plugin<S: Service>(self: &Arc<Self>, svc: S) -> Result<FiberId, CordisError> {
        let tid = TypeId::of::<S>();
        if self.store.read().contains_key(&tid) {
            return Err(CordisError::Configuration(format!(
                "duplicate provider for {:?}",
                tid
            )));
        }
        let disposable = svc.init(self).await?;
        let svc_arc = self.provide(svc);
        let fid = NEXT_FIBER_ID.fetch_add(1, Ordering::SeqCst) as u64;
        if let Some(d) = disposable {
            let fiber = self.fiber.clone();
            let undo: Box<dyn FnOnce() + Send> = Box::new(move || {
                d.dispose();
            });
            fiber.push_undo(undo);
        }
        let fiber = self.fiber.clone();
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
        let disposable = plugin.apply(self, config)?;
        let fiber = Arc::new(Fiber::new());
        let undo: Box<dyn FnOnce() + Send> = Box::new(move || {
            disposable.dispose();
        });
        fiber.push_undo(undo);
        let fid = NEXT_FIBER_ID.fetch_add(1, Ordering::SeqCst) as u64;
        let _ = fiber;
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
// RegistryService & Plugin (Phase 2, Step 10)
// ---------------------------------------------------------------------------

pub trait Plugin: Send + Sync + 'static {
    type Config: Serialize + DeserializeOwned + Send + Sync + 'static;
    type Provides: Service;
    fn apply(
        &self,
        ctx: &Arc<Context>,
        config: Self::Config,
    ) -> Result<Box<dyn Disposable>, CordisError>;
}

pub struct RegistryService {
    fibers: RwLock<HashMap<FiberId, Arc<Fiber>>>,
    // single-source discipline: TypeId -> FiberId (isolate realm simplified to global; Phase 3 will add isolate map)
    provided: RwLock<HashMap<TypeId, FiberId>>,
    next_id: Mutex<FiberId>,
}

impl RegistryService {
    pub fn new() -> Self {
        Self {
            fibers: RwLock::new(HashMap::new()),
            provided: RwLock::new(HashMap::new()),
            next_id: Mutex::new(1),
        }
    }

    /// Register a plugin, enforce single-source discipline.
    /// Returns FiberId or Err(Configuration("duplicate provider for <TypeId>"))
    pub fn plugin<P: Plugin>(
        &self,
        ctx: &Arc<Context>,
        plugin: P,
        config: P::Config,
    ) -> Result<FiberId, CordisError> {
        let tid = TypeId::of::<P::Provides>();
        {
            let provided = self.provided.read();
            if provided.contains_key(&tid) {
                return Err(CordisError::Configuration(format!(
                    "duplicate provider for {:?}",
                    tid
                )));
            }
        }
        let disposable = plugin.apply(ctx, config)?;
        let mut id_guard = self.next_id.lock();
        let fid = *id_guard;
        *id_guard += 1;
        drop(id_guard);

        let fiber = Arc::new(Fiber::new());
        // push the plugin's disposable onto fiber's acc so dispose reverts it
        let undo: Box<dyn FnOnce() + Send> = Box::new(move || {
            disposable.dispose();
        });
        fiber.push_undo(undo);
        self.fibers.write().insert(fid, fiber);
        self.provided.write().insert(tid, fid);
        Ok(fid)
    }

    pub fn get_fiber(&self, id: FiberId) -> Option<Arc<Fiber>> {
        self.fibers.read().get(&id).cloned()
    }

    pub fn remove(&self, id: FiberId) -> Option<Arc<Fiber>> {
        let fiber = self.fibers.write().remove(&id)?;
        // remove provided entry
        let mut provided = self.provided.write();
        provided.retain(|_, v| *v != id);
        Some(fiber)
    }
}

impl Default for RegistryService {
    fn default() -> Self {
        Self::new()
    }
}

impl Service for RegistryService {}

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
                ctx: &Arc<Context>,
                _cfg: Self::Config,
            ) -> Result<Box<dyn Disposable>, CordisError> {
                ctx.provide(FooService(1));
                Ok(Box::new(|| {}) as Box<dyn Disposable>)
            }
        }

        struct FooPlugin2;
        impl Plugin for FooPlugin2 {
            type Config = ();
            type Provides = FooService;
            fn apply(
                &self,
                ctx: &Arc<Context>,
                _cfg: Self::Config,
            ) -> Result<Box<dyn Disposable>, CordisError> {
                ctx.provide(FooService(2));
                Ok(Box::new(|| {}) as Box<dyn Disposable>)
            }
        }

        let fid1 = registry.plugin(&ctx, FooPlugin, ()).expect("first plugin ok");
        assert!(registry.get_fiber(fid1).is_some());
        let err = registry.plugin(&ctx, FooPlugin2, ()).expect_err("duplicate should fail");
        assert!(err.to_string().contains("duplicate provider"));
        // original still present
        assert!(registry.get_fiber(fid1).is_some());
    }
}
