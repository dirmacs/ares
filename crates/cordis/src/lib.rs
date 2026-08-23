#![allow(missing_docs)]
#![allow(dead_code)]

use parking_lot::RwLock;
use std::any::TypeId;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Weak};
use tokio::sync::watch;

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
pub fn inventory_len() -> usize {
    inventory::iter::<CordisInventory>.into_iter().count()
}

#[cfg(not(feature = "inventory"))]
pub fn inventory_len() -> usize {
    0
}

pub mod context;
pub mod effect;
pub mod events;
pub mod fiber;
pub mod service;

pub use context::Context;
pub use effect::{Disposable, Effect, EffectGuard};
pub use events::{Dispatch, EventsService};
pub use fiber::{Fiber, FiberState};
pub use service::{CordisError, Service, ServiceInitFuture};

pub mod events_catalog;
pub use events_catalog::{contract_for, validate_dispatch, validate_listener, EventContract};
pub mod loader;
pub use loader::{Entry, EntryTree, Loader};

pub mod hmr;
pub mod registry;
pub mod watcher;
pub use registry::{Plugin, RegistryService};

#[cfg(feature = "rhai")]
pub mod rhai_service;
#[cfg(feature = "rhai")]
pub use rhai_service::{RhaiPlugin, RhaiService, RhaiServiceConfig};

pub type Symbol = String;
pub type EventId = String;
pub type FiberId = u64;

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

/// Register kernel string factories consumed by the declarative loader.
pub fn register_plugins(reg: &PluginRegistry) {
    reg.register(
        "EventsService",
        Arc::new(|ctx, _config| {
            let future = ctx.plugin(EventsService::new());
            tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
        }),
    );
}

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
                    "event": crate::events_catalog::ev::SERVICE_CHANGED
                });
                tokio::spawn(async move {
                    let _ = events
                        .dispatch(
                            crate::events_catalog::ev::SERVICE_CHANGED.to_string(),
                            payload,
                            Dispatch::Emit,
                        )
                        .await;
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
    use parking_lot::Mutex;

    #[test]
    fn inventory_len_is_kernel_only() {
        #[cfg(feature = "inventory")]
        assert_eq!(inventory_len(), 4);
        #[cfg(not(feature = "inventory"))]
        assert_eq!(inventory_len(), 0);
    }

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
        assert_eq!(consumer_fiber.state(), FiberState::Inactive { error: None });
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
            .dispatch(
                "test".into(),
                serde_json::Value::Number(1.into()),
                Dispatch::Serial,
            )
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

        let fid1 = registry
            .plugin(&ctx, FooPlugin, ())
            .expect("first plugin ok");
        assert!(registry.get_fiber(fid1).is_some());
        let err = registry
            .plugin(&ctx, FooPlugin2, ())
            .expect_err("duplicate should fail");
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
        assert!(matches!(fiber.state(), FiberState::Inactive { .. }));

        // Provide DepService -> fiber should activate via notify cascade
        ctx.provide(DepService);
        // Give tokio a chance to run the spawned refresh
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            matches!(fiber.state(), FiberState::Active { .. }),
            "fiber should be Active after provide, got: {:?}",
            fiber.state()
        );

        // Remove DepService -> fiber should deactivate via notify cascade
        ctx.remove::<DepService>();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            matches!(fiber.state(), FiberState::Inactive { .. }),
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
    async fn isolate_wins_over_same_type_intercept() {
        #[derive(Debug)]
        struct ToolSvc(String);
        impl Service for ToolSvc {}

        #[derive(Debug)]
        struct OtherSvc(String);
        impl Service for OtherSvc {}

        let root = Context::new_root();
        let child = root.isolate::<ToolSvc>("acme");
        child.provide(ToolSvc("store".into()));

        let intercepted = child.intercept(ToolSvc("override".into()));
        assert_eq!(intercepted.get::<ToolSvc>().unwrap().0, "store");

        let mixed = child.intercept(OtherSvc("override".into()));
        assert_eq!(mixed.get::<OtherSvc>().unwrap().0, "override");
        assert_eq!(mixed.get::<ToolSvc>().unwrap().0, "store");
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
    async fn inject_unblocks_via_reflect_notify() {
        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        let reflect = ctx.get::<ReflectService>().unwrap();
        reflect.set_context(&ctx);

        let waiter = ctx.clone();
        let handle = tokio::spawn(async move { waiter.inject::<FooService>().await });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        ctx.provide(FooService(7));
        let got = tokio::time::timeout(std::time::Duration::from_millis(200), handle)
            .await
            .expect("inject should complete within 200ms via reflect notify")
            .expect("inject task should not panic");
        assert_eq!(got.0, 7);
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
        let mut bus_rx = svc.subscribe();

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
        let (evt, bus_payload) =
            tokio::time::timeout(std::time::Duration::from_secs(1), bus_rx.recv())
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
        let svc = EventsService::new();
        let payload = serde_json::json!({ "n": 1 });
        let seen = Arc::new(Mutex::new(Vec::new()));

        for tag in ["a", "b", "c"] {
            let seen = seen.clone();
            let tag = tag.to_string();
            svc.on("serial.test".into(), move |received| {
                let seen = seen.clone();
                let tag = tag.clone();
                async move {
                    seen.lock().push((tag, received));
                    Ok(serde_json::Value::Null)
                }
            });
        }

        let out = svc
            .dispatch("serial.test".into(), payload.clone(), Dispatch::Serial)
            .await
            .unwrap();

        // Serial handlers see the original payload, and an all-null chain preserves it.
        assert_eq!(out, payload);
        assert_eq!(
            seen.lock().clone(),
            vec![
                ("a".to_string(), payload.clone()),
                ("b".to_string(), payload.clone()),
                ("c".to_string(), payload),
            ]
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
        svc.on_waterfall("wf.next".into(), |payload, _next| async move {
            let mut obj = payload.as_object().cloned().unwrap_or_default();
            obj.insert("inner_seen".into(), serde_json::json!(payload.get("value")));
            Ok(serde_json::Value::Object(obj))
        });

        let payload = serde_json::json!({ "value": 42 });
        let out = svc
            .dispatch("wf.next".into(), payload, Dispatch::Waterfall)
            .await
            .unwrap();
        let obj = out
            .as_object()
            .expect("waterfall output should be an object");
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
        let payload = serde_json::json!({ "n": 1 });
        let seen = Arc::new(Mutex::new(Vec::new()));

        for tag in ["a", "b"] {
            let seen = seen.clone();
            let tag = tag.to_string();
            svc.on("par2.test".into(), move |received| {
                let seen = seen.clone();
                let tag = tag.clone();
                async move {
                    seen.lock().push((tag, received));
                    Ok(serde_json::json!({ "handler": "complete" }))
                }
            });
        }

        let out = svc
            .dispatch("par2.test".into(), payload.clone(), Dispatch::Parallel)
            .await
            .unwrap();

        // Parallel waits for every handler, but successful dispatch returns null.
        assert_eq!(out, serde_json::Value::Null);
        let mut completed = seen.lock().clone();
        completed.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            completed,
            vec![("a".to_string(), payload.clone()), ("b".to_string(), payload)]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn notify_broadcasts_service_changed_event() {
        let ctx = Context::new_root();
        let events_handle = ctx.provide(EventsService::new());
        let reflect = ctx.provide(ReflectService::new());
        reflect.set_context(&ctx);

        let mut rx = events_handle.subscribe();
        reflect.notify(TypeId::of::<u64>());

        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(500);
        let mut seen = false;
        while std::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok((name, payload)) => {
                    assert_eq!(name, crate::events_catalog::ev::SERVICE_CHANGED);
                    // TypeId formats as a hash, not a name; just require presence.
                    assert!(
                        payload["type_id"].as_str().unwrap().starts_with("TypeId("),
                        "payload should identify the changed type: {payload}"
                    );
                    seen = true;
                    break;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                Err(e) => panic!("unexpected broadcast error: {e}"),
            }
        }
        assert!(seen, "service.changed broadcast not observed within timeout");
    }
}
