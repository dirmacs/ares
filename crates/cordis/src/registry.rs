use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde::{de::DeserializeOwned, Serialize};

use crate::{Context, CordisError, Fiber, FiberId, Service, Symbol};

/// A plugin is a declarative unit of configuration that can provide a service
/// into a context. The registry calls apply to create the service value and
/// then inserts it into the context with single-source discipline. The
/// returned Arc is the live service instance that callers retrieve through
/// ctx.get. Plain prose is used here to keep the docs readable and to avoid
/// clever abstractions that would hide the simple flow of create then provide.
pub trait Plugin: Send + Sync + 'static {
    type Config: Serialize + DeserializeOwned + Send + Sync + 'static;
    type Provides: Service;
    fn apply(
        &self,
        ctx: &Arc<Context>,
        config: Self::Config,
    ) -> Result<Arc<Self::Provides>, CordisError>;
}

/// Composable readiness predicate for [`RegistryService::register_with_readiness`]
/// (C2 `ready_when`).
///
/// A barrier is a shared closure consulted by the lifecycle on every
/// activation pass. While it reports `false` the fiber rests inspectable
/// `Pending` — quiet waiting, NOT failure: availability predicates
/// ([`Service::check`]) are the loud complement that rests
/// `Failed{error: "availability predicate rejected service"}` and converge
/// through refreshes; a readiness gate simply holds the fiber out of
/// service until the environment it observes turns ready.
///
/// Re-kick contract: a gated fiber re-evaluates whenever its lifecycle is
/// driven — in practice via the existing round-5 observer fan-out, because
/// any managed fiber settling (`Active`/`Inactive`/`Pending`/`Failed`) ends
/// in provider/withdrawal notifies that BFS-refresh dependents. Barriers
/// therefore observe plain context facts (`ctx.get::<T>().is_some()`,
/// versions, config values) rather than registering their own watchers.
/// Shared readiness predicate handle.
type ReadinessPredicate = Arc<dyn Fn(&Arc<Context>) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct ReadinessBarrier {
    inner: ReadinessPredicate,
    /// TypeIds whose provider settlements re-kick the gated fiber.
    keys: Vec<TypeId>,
}

impl ReadinessBarrier {
    /// Wrap one readiness predicate.
    pub fn new(ready: impl Fn(&Arc<Context>) -> bool + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(ready),
            keys: Vec::new(),
        }
    }

    /// The declared watch keys for re-kick fan-out registration.
    pub fn watched_type_ids(&self) -> &[TypeId] {
        &self.keys
    }

    /// Evaluate the composed predicate against a context.
    pub fn is_ready(&self, ctx: &Arc<Context>) -> bool {
        (self.inner)(ctx)
    }

    /// AND-composition helper (`with_readiness`): the combined barrier is
    /// ready only when BOTH operands report ready. Short-circuits on the
    /// first closed half.
    pub fn and(self, other: ReadinessBarrier) -> ReadinessBarrier {
        let pair = (self.inner, other.inner);
        ReadinessBarrier::new(move |ctx| (pair.0)(ctx) && (pair.1)(ctx))
            .watching(self.keys.iter().chain(other.keys.iter()).copied())
    }

    /// Declare the `TypeId`s whose provider settlements should re-kick a
    /// fiber gated by this barrier (see
    /// [`RegistryService::register_with_readiness`]). Composition unions
    /// both sides.
    pub fn watching(
        mut self,
        keys: impl IntoIterator<Item = TypeId>,
    ) -> ReadinessBarrier {
        self.keys.extend(keys);
        self
    }
}

/// AND-composition helper (`with_readiness`): combine any number of
/// readiness predicates into one [`ReadinessBarrier`] that is ready only
/// when every operand is ready. `with_readiness([a, b, c])` reads as
/// "ready when a AND b AND c"; the empty slice is vacuously ready.
pub fn with_readiness(
    barriers: impl IntoIterator<Item = ReadinessBarrier>,
) -> ReadinessBarrier {
    let mut combined: Option<ReadinessBarrier> = None;
    for barrier in barriers {
        combined = Some(match combined {
            None => barrier,
            Some(acc) => acc.and(barrier),
        });
    }
    combined.unwrap_or_else(|| ReadinessBarrier::new(|_ctx| true))
}

impl std::fmt::Debug for ReadinessBarrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadinessBarrier").finish_non_exhaustive()
    }
}

/// RegistryService tracks fibers and enforces that only one fiber may provide
/// a given TypeId inside the same isolate realm. If two registrations try to
/// provide the same service type while their isolate labels overlap, the
/// second registration fails with a configuration error that contains the
/// phrase duplicate provider for and the debug form of the TypeId. Different
/// isolate labels are allowed to provide the same TypeId because they live in
/// disjoint realms, which is how multi-tenant tool and agent isolation is
/// implemented. Fibers are stored by id so that callers can inspect or remove
/// them later, and the provided map is keyed by the pair of TypeId and
/// isolate label.
pub struct RegistryService {
    fibers: RwLock<HashMap<FiberId, Arc<Fiber>>>,
    provided: RwLock<HashMap<(TypeId, Option<Symbol>), FiberId>>,
    /// Registration context per tracked fiber, used to resolve which isolate
    /// realm a consumer was registered against (guarded withdrawal).
    realms: RwLock<HashMap<FiberId, std::sync::Weak<Context>>>,
    next_id: Mutex<FiberId>,
}

impl RegistryService {
    pub fn new() -> Self {
        Self {
            fibers: RwLock::new(HashMap::new()),
            provided: RwLock::new(HashMap::new()),
            realms: RwLock::new(HashMap::new()),
            next_id: Mutex::new(1),
        }
    }

    /// Count active consumer fibers that currently resolve the provider key
    /// `(TypeId, isolate label)` through this registry.
    ///
    /// Reliance is derived live rather than seeded incrementally: a tracked
    /// fiber counts as a consumer of `key` when it is Active, was registered
    /// against the same isolate realm as the key's label, declares an inject
    /// on the key's TypeId, and is not the provider fiber itself. Late
    /// `declare_inject` calls therefore count immediately, retired or failed
    /// consumers never block a withdrawal, and no stale bookkeeping can
    /// accumulate.
    pub fn reliance_count(&self, key: &(TypeId, Option<Symbol>)) -> usize {
        let fibers = self.fibers.read();
        let mut consumers = 0;
        for (fid, fiber) in fibers.iter() {
            if !matches!(fiber.state(), crate::FiberState::Active { .. }) {
                continue;
            }
            // The provider itself never counts as its own consumer.
            if Some(*fid) == self.provided.read().get(key).copied() {
                continue;
            }
            // Realm check: the consumer must resolve the injected type in the
            // same isolate realm the provider serves. `register` records each
            // fiber's registration context, so its `isolate_label` view is the
            // authoritative realm; loader-tracked fibers (no recorded context)
            // cannot resolve realms and are skipped.
            let Some(reg_ctx) = self
                .realms
                .read()
                .get(fid)
                .and_then(std::sync::Weak::upgrade)
            else {
                continue;
            };
            if reg_ctx.isolate_label(key.0) != key.1 {
                continue;
            }
            for inject_tid in fiber.injected_type_ids() {
                if inject_tid == key.0 {
                    consumers += 1;
                    break;
                }
            }
        }
        consumers
    }

    fn next_fiber_id(&self) -> FiberId {
        let mut guard = self.next_id.lock();
        let id = *guard;
        *guard += 1;
        id
    }

    /// Register a plugin and provide its service into the context. The call
    /// first checks the single-source map for an existing provider with the
    /// same TypeId and overlapping isolate realm. Overlap means the isolate
    /// labels are equal, including both being None for the root realm. If a
    /// duplicate is found the function returns a configuration error. Otherwise
    /// it calls the plugin to build the service, inserts the service into the
    /// context, creates a fiber to represent the registration, and records the
    /// mapping.
    ///
    /// Availability predicates: the plugin factory's product is consulted via
    /// [`Service::check`] before it is provided. A not-ready instance rests
    /// the fiber as inspectable `Failed { error: "availability predicate
    /// rejected service" }` instead of `Active`; registration itself stays
    /// non-throwing because register-before-ready is a supported transient
    /// that later refreshes converge (see the metatheory confluence legs).
    pub fn register<P: Plugin>(
        &self,
        ctx: &Arc<Context>,
        plugin: P,
        config: P::Config,
    ) -> Result<FiberId, CordisError> {
        let tid = TypeId::of::<P::Provides>();
        let isolate = ctx.isolate_label(tid);
        let key = (tid, isolate);
        // Decide staleness under the read guard only — the guard is a
        // scrutinee temporary that would otherwise outlive the if-let body,
        // deadlocking the `provided.write()` below against its own reader.
        let active_conflict = match self.provided.read().get(&key).copied() {
            Some(existing) => self
                .fibers
                .read()
                .get(&existing)
                .map(|fiber| {
                    matches!(
                        fiber.state(),
                        crate::FiberState::Active { .. }
                            | crate::FiberState::Loading
                            | crate::FiberState::Reloading
                    )
                })
                .unwrap_or(false),
            None => false,
        };
        if active_conflict {
            return Err(CordisError::DuplicateProvider {
                name: format!("{tid:?}"),
                owner: format!("isolate realm {:?}", ctx.isolate_label(tid)),
            });
        }
        // A non-conflicting entry is stale (retired/disposed/failed provider)
        // and must not block the fresh registration.
        self.provided.write().remove(&key);

        let fid = self.next_fiber_id();
        let fiber = Arc::new(Fiber::new());
        fiber.set_state(crate::FiberState::Loading);
        fiber.set_reload_context(ctx);
        fiber.set_id(fid);
        self.fibers.write().insert(fid, fiber.clone());
        self.realms.write().insert(fid, Arc::downgrade(ctx));

        let config_value = serde_json::to_value(&config).map_err(|error| {
            let message = format!("cannot serialize plugin config: {error}");
            fiber.set_state(crate::FiberState::Failed {
                error: Some(message.clone()),
            });
            self.wire_failed_registration(ctx, fid, &fiber, tid);
            CordisError::Configuration(message)
        })?;
        // C1 intercept meta-events: stage the raw config so every refresh
        // pass re-resolves the EFFECTIVE config from a single source.
        fiber.set_raw_config(config_value.clone());
        // C1 `internal/config` covers the ACTIVATION path too, not just
        // refresh passes: resolve the effective config ONCE here so the very
        // first runner pass below applies the intercepted configuration. A
        // chain error fails the activation (`Failed`), mirroring refresh.
        if let Some(events) = ctx.get::<crate::EventsService>() {
            match crate::events::blocking_intercept_config(&events, config_value.clone()) {
                Ok(effective) => {
                    if !effective.is_null() {
                        fiber.stage_effective_config(effective);
                    }
                }
                Err(error) => {
                    fiber.set_state(crate::FiberState::Failed {
                        error: Some(error.to_string()),
                    });
                    self.wire_failed_registration(ctx, fid, &fiber, tid);
                    return Err(error);
                }
            }
        }
        let plugin = Arc::new(plugin);
        let weak_fiber = Arc::downgrade(&fiber);
        fiber.set_reload_runner(Box::new(move |ctx| {
            // C1 intercept meta-events: the interception point stages the
            // effective config for this pass; without one this is the raw
            // config and the path is byte-identical to the legacy runner.
            // (The weak handle is upgraded first so a dropped registration
            // fiber still falls back to the raw config instead of erroring.)
            let cfg_raw = weak_fiber
                .upgrade()
                .and_then(|owner| owner.effective_config_override())
                .unwrap_or_else(|| config_value.clone());
            let config =
                serde_json::from_value::<P::Config>(cfg_raw).map_err(|error| {
                    CordisError::Configuration(format!("cannot deserialize plugin config: {error}"))
                })?;
            let owner = weak_fiber
                .upgrade()
                .ok_or_else(|| CordisError::Fiber("registration fiber was dropped".into()))?;
            // Panic containment: a panicking plugin factory must not tear the
            // host down. `AssertUnwindSafe` is required — the closure borrows
            // `plugin` and the deserialized `config` by reference, which the
            // unwinding cannot leave observably broken here because every
            // captured value is dropped on unwind and the fiber's state is
            // set to `Failed` below (ledger row #1: inspectable terminal
            // state).
            let applied = crate::hmr::catch_plugin_panic(std::panic::AssertUnwindSafe(|| {
                ctx.with_provider_fiber(&owner, || plugin.apply(ctx, config))
            }));
            let applied = match applied {
                Ok(applied) => applied,
                Err(payload) => {
                    return Err(CordisError::Fiber(format!(
                        "plugin factory panicked: {payload}"
                    )))
                }
            };
            let provides = applied?;
            let healthy = provides.check();
            ctx.provide_on_fiber(provides, &owner);
            Ok(healthy)
        }));

        let healthy = match fiber.run_runner(ctx) {
            Ok(healthy) => healthy,
            Err(error) => {
                fiber.set_state(crate::FiberState::Failed {
                    error: Some(error.to_string()),
                });
                // Metatheory delta #2 (resolved): a failed factory still
                // enters the bookkeeping graph. Dependents of the attempted
                // key observe the provider loss through the same notify path
                // successful registrations use, and the Failed fiber stays
                // inspectable via get_fiber.
                self.wire_failed_registration(ctx, fid, &fiber, tid);
                return Err(error);
            }
        };
        // Availability predicate: the reload runner already consulted
        // `provides.check()` BEFORE the value was provided, so an unready
        // instance was never visible to consumers. Registration stays
        // non-throwing (register-before-ready is a supported transient that
        // later refreshes converge); the fiber rests as an inspectable
        // `Failed` naming the rejection instead of a bare `Inactive`, so
        // operators see WHY it is down.
        let epoch = fiber.compute_epoch(ctx);
        fiber.set_epoch(epoch.clone());
        // NOTE: `mark_applied` is intentionally NOT called here. Reactive
        // `Pending` eligibility requires a FULLY-SATISFIED refresh pass (all
        // declared injects available), which registration cannot prove — a
        // factory may succeed while its declares are still unserved. The
        // flag is set by [`crate::Fiber::refresh`] on its first all-green
        // pass.
        fiber.set_state(if healthy {
            crate::FiberState::Active { epoch }
        } else {
            crate::FiberState::Failed {
                error: Some("availability predicate rejected service".into()),
            }
        });
        self.provided.write().insert(key, fid);

        if let Some(reflect) = ctx.get::<crate::ReflectService>() {
            for dependency in fiber.injected_type_ids() {
                reflect.register_dependent(dependency, fid);
            }
            let _ = reflect.ensure_notifier(tid);
            reflect.register_fiber(fid, fiber.clone(), tid);
        }
        Ok(fid)
    }

    /// Register a plugin with a C2 `ready_when` readiness gate.
    ///
    /// Identical to [`Self::register`] except the fiber carries a
    /// [`ReadinessBarrier`]: while the composed predicate reports not-ready
    /// the lifecycle rests the fiber as inspectable
    /// [`crate::FiberState::Pending`] (quiet waiting — never `Failed`), and
    /// every subsequent refresh/re-kick re-evaluates it. The factory still
    /// runs once at registration (so config errors surface immediately);
    /// a closed gate simply keeps the produced service OUT of consumer
    /// reach until the gate opens, because strict `ctx.get` refuses values
    /// owned by non-`Active` fibers.
    pub fn register_with_readiness<P: Plugin>(
        &self,
        ctx: &Arc<Context>,
        plugin: P,
        config: P::Config,
        ready_when: ReadinessBarrier,
    ) -> Result<FiberId, CordisError> {
        let fid = self.register(ctx, plugin, config)?;
        if let Some(fiber) = self.get_fiber(fid) {
            // Re-kick wiring: the gate may observe provider facts beyond the
            // fiber's own declared injects. Register the fiber against the
            // barrier's declared watch keys so any settle on those types —
            // provide or withdrawal through ReflectService — BFS-refreshes
            // this fiber too.
            for tid in ready_when.watched_type_ids().iter().copied() {
                if let Some(reflect) = ctx.get::<crate::ReflectService>() {
                    reflect.register_dependent(tid, fid);
                    let _ = reflect.ensure_notifier(tid);
                }
            }
            fiber.set_readiness_gate(ready_when);
            // Re-enter the lifecycle so the freshly-installed gate decides
            // the resting state right away instead of waiting for an
            // external kick. Registration left the fiber `Active`/`Failed`;
            // the gate consult runs inside the guarded transition.
            tokio::spawn({
                let fiber = fiber.clone();
                let ctx = ctx.clone();
                async move { fiber.refresh(&ctx).await }
            });
        }
        Ok(fid)
    }

    /// Bookkeeping for a registration that ended in `Failed`.
    ///
    /// The fiber keeps its terminal visible state but must not vanish from
    /// the graph: it stays tracked under its id, is registered with
    /// [`crate::ReflectService`] against the attempted provider key, and a
    /// notify on that key fans out so dependents observe the provider loss
    /// reactively. The `provided` slot stays vacant — a later successful
    /// registration of the same key therefore allocates a fresh fiber id
    /// instead of being refused as a duplicate.
    fn wire_failed_registration(
        &self,
        ctx: &Arc<Context>,
        fid: FiberId,
        fiber: &Arc<Fiber>,
        tid: TypeId,
    ) {
        if let Some(reflect) = ctx.get::<crate::ReflectService>() {
            for dependency in fiber.injected_type_ids() {
                reflect.register_dependent(dependency, fid);
            }
            let _ = reflect.ensure_notifier(tid);
            reflect.register_fiber(fid, fiber.clone(), tid);
            reflect.notify(tid);
        }
    }

    /// Alias for register that matches the historical name used in the
    /// Cordis paper and earlier spike code. New code can use register, old
    /// code can keep calling plugin, both do the same isolate-aware check.
    pub fn plugin<P: Plugin>(
        &self,
        ctx: &Arc<Context>,
        plugin: P,
        config: P::Config,
    ) -> Result<FiberId, CordisError> {
        self.register(ctx, plugin, config)
    }

    /// Track an externally-created registration fiber under a fresh id.
    ///
    /// The loader uses this so plugin-factory fibers created outside
    /// [`RegistryService::register`](Self::register) still resolve through
    /// [`Self::get_fiber`] for retirement/disposal.
    pub fn track_fiber(&self, fiber: Arc<Fiber>) -> FiberId {
        let fid = self.next_fiber_id();
        self.fibers.write().insert(fid, fiber);
        fid
    }

    /// Drop tracking entries for fibers whose disposal already ran.
    ///
    /// A fiber is prunable only when [`Fiber::is_disposed`] is true (the
    /// `disposed` flag `Fiber::dispose` sets). Every other state is kept:
    /// `Failed{error}` is inspectable by design (ledger row #1), and
    /// `Inactive`/`Active`/transitional fibers are live bookkeeping. The
    /// matching `provided` slot and realm record are cleared alongside, so a
    /// fresh registration of the same key never sees a stale conflict.
    pub fn prune_disposed(&self) -> usize {
        let disposed: Vec<FiberId> = self
            .fibers
            .read()
            .iter()
            .filter(|(_, fiber)| fiber.is_disposed())
            .map(|(fid, _)| *fid)
            .collect();
        let mut removed = 0;
        for fid in disposed {
            if self.remove(fid).is_some() {
                removed += 1;
            }
        }
        removed
    }

    pub fn get_fiber(&self, id: FiberId) -> Option<Arc<Fiber>> {
        // Inspectable-by-design read: no implicit pruning. Disposed fibers
        // stay resolvable here until an explicit/opportunistic
        // `prune_disposed()` runs, so post-dispose assertions and refreshes
        // keep working.
        self.fibers.read().get(&id).cloned()
    }

    pub fn remove(&self, id: FiberId) -> Option<Arc<Fiber>> {
        let fiber = self.fibers.write().remove(&id)?;
        let mut provided = self.provided.write();
        provided.retain(|_, v| *v != id);
        drop(provided);
        self.realms.write().remove(&id);
        Some(fiber)
    }

    /// Record the registration context for an externally-created fiber
    /// (loader-tracked registrations), so guarded-withdrawal realm checks can
    /// resolve its isolate realm.
    pub fn track_fiber_in_realm(&self, fid: FiberId, ctx: &Arc<Context>) {
        self.realms.write().insert(fid, Arc::downgrade(ctx));
    }

    /// Resolved provider fiber ids currently serving the given service types
    /// in `ctx`'s isolate realms (C2 cascade batching lookup).
    pub fn provider_fibers_for(&self, ctx: &Arc<Context>, tids: &[TypeId]) -> Vec<u64> {
        let provided = self.provided.read();
        tids.iter()
            .filter_map(|tid| {
                let isolate = ctx.isolate_label(*tid);
                provided.get(&(*tid, isolate)).copied()
            })
            .collect()
    }

    /// The service types whose live provider slot is owned by `fid`
    /// (C2 cascade batching: post-settle re-kick fan-out).
    pub fn provided_types_of_fiber(&self, fid: FiberId) -> Vec<TypeId> {
        self.provided
            .read()
            .iter()
            .filter(|(_, owner)| **owner == fid)
            .map(|(key, _)| key.0)
            .collect()
    }

    /// Snapshot of every currently tracked fiber id (introspection surface).
    pub fn tracked_ids(&self) -> Vec<FiberId> {
        self.fibers.read().keys().copied().collect()
    }

    pub fn len(&self) -> usize {
        // Opportunistic dead-fiber sweep: every cheap size probe also drops
        // entries whose disposal already ran.
        self.prune_disposed();
        self.fibers.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.fibers.read().is_empty()
    }
}

impl Default for RegistryService {
    fn default() -> Self {
        Self::new()
    }
}

impl Service for RegistryService {}

// Static registration for compile-time plugins. The inventory crate collects
// CordisInventory entries at link time, while linkme offers a similar
// distributed slice. Both are optional and off by default for the core crate,
// and the registry itself is the real runtime surface that enforces
// single-source discipline. RegistryService is submitted once from lib.rs.

#[cfg(feature = "linkme")]
#[linkme::distributed_slice]
pub static REGISTRY_PLUGINS: [fn(&Arc<Context>) -> Result<FiberId, CordisError>];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Context, FiberState, ReflectService, Service};

    #[derive(Debug)]
    struct FooService(pub i32);
    impl Service for FooService {}

    #[derive(Debug)]
    struct BarService(pub i32);
    impl Service for BarService {}

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

    struct BarPlugin;
    impl Plugin for BarPlugin {
        type Config = ();
        type Provides = BarService;
        fn apply(
            &self,
            _ctx: &Arc<Context>,
            _cfg: Self::Config,
        ) -> Result<Arc<Self::Provides>, CordisError> {
            Ok(Arc::new(BarService(99)))
        }
    }

    #[test]
    fn duplicate_provider_rejected() {
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        let fid1 = registry
            .register(&ctx, FooPlugin, ())
            .expect("first registration should succeed");
        assert!(registry.get_fiber(fid1).is_some());
        // Second plugin providing same TypeId in same isolate realm must fail.
        let err = registry
            .register(&ctx, FooPlugin2, ())
            .expect_err("duplicate provider should be rejected");
        assert!(
            err.to_string().contains("duplicate provider for"),
            "error should mention duplicate provider, got {err}"
        );
        // Original still retrievable.
        assert!(registry.get_fiber(fid1).is_some());
        // Service still retrievable through context.
        let svc = ctx.get::<FooService>().expect("service should be present");
        assert_eq!(svc.0, 1);
    }

    struct FailingPlugin;
    impl Plugin for FailingPlugin {
        type Config = ();
        type Provides = FooService;
        fn apply(
            &self,
            _ctx: &Arc<Context>,
            _cfg: Self::Config,
        ) -> Result<Arc<Self::Provides>, CordisError> {
            Err(CordisError::Configuration("intentional failure".into()))
        }
    }

    #[test]
    fn failed_plugin_transitions_tracked_fiber_to_failed_with_error() {
        use crate::FiberState;
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        // A failing plugin is rejected, but the registry still tracked a fiber
        // in the Failed state carrying the error.
        let err = registry
            .register(&ctx, FailingPlugin, ())
            .expect_err("failing plugin should be rejected");
        assert!(err.to_string().contains("intentional failure"));
        let existing = registry
            .get_fiber(1)
            .expect("failed fiber should be tracked");
        match existing.state() {
            FiberState::Failed { error } => {
                assert!(error
                    .as_deref()
                    .unwrap_or("")
                    .contains("intentional failure"));
            }
            other => panic!("expected Failed state, got {other:?}"),
        }
        // No service was provided for the failing plugin.
        assert!(ctx.get::<FooService>().is_none());
    }

    struct PanickingPlugin;
    impl Plugin for PanickingPlugin {
        type Config = ();
        type Provides = FooService;
        fn apply(
            &self,
            _ctx: &Arc<Context>,
            _cfg: Self::Config,
        ) -> Result<Arc<Self::Provides>, CordisError> {
            panic!("factory exploded");
        }
    }

    /// A panicking factory must not abort the host: register converts the
    /// unwind into an inspectable `Failed` fiber carrying "factory panicked",
    /// keeps the provider key unserved, and notifies dependents through the
    /// same path as any other failed registration.
    #[tokio::test]
    async fn panicking_factory_registers_failed_and_notifies_dependents() {
        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        if let Some(reflect) = ctx.get::<ReflectService>() {
            reflect.set_context(&ctx);
        }
        let registry = RegistryService::new();

        let err = registry
            .register(&ctx, PanickingPlugin, ())
            .expect_err("panicking factory should be rejected");
        match &err {
            CordisError::Fiber(message) => {
                assert!(
                    message.contains("plugin factory panicked"),
                    "unexpected error text: {message}"
                );
                assert!(message.contains("factory exploded"));
            }
            other => panic!("expected Fiber error, got {other:?}"),
        }
        // The fiber stays inspectable with the panic message (ledger row #1).
        let fiber = registry.get_fiber(1).expect("failed fiber is tracked");
        match fiber.state() {
            FiberState::Failed { error } => {
                let error = error.as_deref().unwrap_or("");
                assert!(error.contains("factory panicked"), "got: {error}");
                assert!(error.contains("factory exploded"));
            }
            other => panic!("expected Failed state, got {other:?}"),
        }
        // No service was provided; the key stays unserved for a fresh try.
        assert!(ctx.get::<FooService>().is_none());
        let fid_retry = registry
            .register(&ctx, FooPlugin, ())
            .expect("fresh registration of the same key after a panic");
        assert!(matches!(
            registry.get_fiber(fid_retry).unwrap().state(),
            FiberState::Active { .. }
        ));

        // Dependents observe the loss through the reactive notify path:
        // declare an inject against the attempted key after the failure and
        // refresh — it resolves only once the passing registration lands.
        let dep_fid = registry
            .register(&ctx, BarPlugin, ())
            .expect("dependent registers without its dependency");
        let dependent = registry.get_fiber(dep_fid).unwrap();
        dependent.declare_inject::<FooService>();
        dependent.refresh(&ctx).await;
        assert!(matches!(dependent.state(), FiberState::Active { .. }));
    }

    #[test]
    fn re_registered_good_plugin_moves_fiber_to_active() {
        use crate::FiberState;
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        let _ = registry
            .register(&ctx, FailingPlugin, ())
            .expect_err("failing plugin should be rejected");
        let original = registry.get_fiber(1).unwrap();
        assert!(matches!(original.state(), FiberState::Failed { .. }));

        // A subsequent good registration (different type) is a new fiber, Active.
        let fid_ok = registry
            .register(&ctx, BarPlugin, ())
            .expect("good plugin should register");
        let ok = registry.get_fiber(fid_ok).expect("good fiber");
        match ok.state() {
            FiberState::Active { .. } => {}
            other => panic!("expected Active state, got {other:?}"),
        }
    }

    #[test]
    fn different_isolates_allowed() {
        let root = Context::new_root();
        let registry = RegistryService::new();
        // First provider in root realm.
        let fid_root = registry
            .register(&root, FooPlugin, ())
            .expect("root registration ok");
        assert!(registry.get_fiber(fid_root).is_some());
        // Isolate for FooService with label tenant acme.
        let tenant_a = root.isolate::<FooService>("tenant:acme");
        let fid_a = registry
            .register(&tenant_a, FooPlugin2, ())
            .expect("different isolate should be allowed");
        assert!(registry.get_fiber(fid_a).is_some());
        // Yet a second registration in the same tenant isolate must fail.
        struct FooPlugin3;
        impl Plugin for FooPlugin3 {
            type Config = ();
            type Provides = FooService;
            fn apply(
                &self,
                _ctx: &Arc<Context>,
                _cfg: Self::Config,
            ) -> Result<Arc<Self::Provides>, CordisError> {
                Ok(Arc::new(FooService(3)))
            }
        }
        let err = registry
            .register(&tenant_a, FooPlugin3, ())
            .expect_err("duplicate in same isolate should fail");
        assert!(err.to_string().contains("duplicate provider for"));
        // Different isolate label is allowed.
        let tenant_b = root.isolate::<FooService>("tenant:other");
        let fid_b = registry
            .register(&tenant_b, FooPlugin3, ())
            .expect("different isolate label should be allowed");
        assert!(registry.get_fiber(fid_b).is_some());
    }

    /// Shared helper that verifies a plugin registers, its fiber is tracked,
    /// and the provided service is retrievable via `ctx.get`. Extracted to
    /// eliminate near-duplicate test bodies (88% alike) reported by rust-doctor.
    fn assert_plugin_retrievable<T, P>(
        registry: &RegistryService,
        ctx: &Arc<Context>,
        plugin: P,
        expect: impl FnOnce(&T),
    ) where
        T: Service + std::fmt::Debug,
        P: Plugin<Provides = T, Config = ()>,
    {
        let fid = registry
            .plugin(ctx, plugin, ())
            .expect("plugin alias should work");
        assert!(registry.get_fiber(fid).is_some());
        let svc = ctx
            .get::<T>()
            .expect("service should be retrievable via ctx.get");
        expect(&svc);
    }

    #[test]
    fn successful_provide_retrievable_via_ctx_get() {
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        assert_plugin_retrievable(&registry, &ctx, BarPlugin, |svc: &BarService| {
            assert_eq!(svc.0, 99);
        });
        // Registry length reflects one fiber.
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn registration_fiber_disposal_removes_only_its_service() {
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        let foo = registry
            .register(&ctx, FooPlugin, ())
            .expect("foo registration");
        let bar = registry
            .register(&ctx, BarPlugin, ())
            .expect("bar registration");
        assert!(ctx.get::<FooService>().is_some());
        assert!(ctx.get::<BarService>().is_some());
        let _ = registry.get_fiber(foo).unwrap().dispose().await;
        assert!(ctx.get::<FooService>().is_none());
        assert_eq!(ctx.get::<BarService>().unwrap().0, 99);
        assert!(matches!(
            registry.get_fiber(bar).unwrap().state(),
            FiberState::Active { .. }
        ));
        registry.get_fiber(foo).unwrap().refresh(&ctx).await;
        assert!(ctx.get::<FooService>().is_none());
    }

    struct Dependency;
    impl Service for Dependency {}

    struct CountingPlugin {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Plugin for CountingPlugin {
        type Config = ();
        type Provides = FooService;

        fn apply(
            &self,
            _ctx: &Arc<Context>,
            _cfg: Self::Config,
        ) -> Result<Arc<Self::Provides>, CordisError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Arc::new(FooService(7)))
        }
    }

    #[tokio::test]
    async fn refresh_reruns_provider_after_dependency_version_change() {
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fid = registry
            .register(
                &ctx,
                CountingPlugin {
                    calls: calls.clone(),
                },
                (),
            )
            .expect("registration");
        let fiber = registry.get_fiber(fid).unwrap();
        fiber.declare_inject::<Dependency>();
        ctx.provide(Dependency);
        fiber.refresh(&ctx).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(matches!(fiber.state(), FiberState::Active { .. }));
    }

    #[tokio::test]
    async fn missing_dependency_deactivates_then_provide_reactivates() {
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        let fid = registry
            .register(&ctx, FooPlugin, ())
            .expect("registration");
        let fiber = registry.get_fiber(fid).unwrap();
        fiber.declare_inject::<Dependency>();
        fiber.refresh(&ctx).await;
        assert!(matches!(fiber.state(), FiberState::Inactive { .. }));
        assert!(ctx.get::<FooService>().is_none());
        ctx.provide(Dependency);
        fiber.refresh(&ctx).await;
        assert!(matches!(fiber.state(), FiberState::Active { .. }));
        assert!(ctx.get::<FooService>().is_some());
    }

    #[test]
    fn isolate_lookup_does_not_cross_realms() {
        let root = Context::new_root();
        root.provide(FooService(1));
        let tenant = root.isolate::<FooService>("tenant:a");
        assert!(tenant.get::<FooService>().is_none());
        tenant.provide(FooService(2));
        assert_eq!(tenant.get::<FooService>().unwrap().0, 2);
        let other = root.isolate::<FooService>("tenant:b");
        assert!(other.get::<FooService>().is_none());
    }

    // --- guarded withdrawal (paper §4.3.1 relied_n) ---

    struct DependentPlugin;
    impl Plugin for DependentPlugin {
        type Config = ();
        type Provides = BarService;
        fn apply(
            &self,
            _ctx: &Arc<Context>,
            _cfg: Self::Config,
        ) -> Result<Arc<Self::Provides>, CordisError> {
            Ok(Arc::new(BarService(5)))
        }
    }

    /// Register a consumer of `FooService` through the registry so it is
    /// Active and declares its inject against a recorded registration context.
    fn register_foo_consumer(ctx: &Arc<Context>, registry: &RegistryService) -> FiberId {
        let fid = registry
            .register(&ctx.clone(), DependentPlugin, ())
            .expect("dependent registration");
        registry
            .get_fiber(fid)
            .unwrap()
            .declare_inject::<FooService>();
        fid
    }

    #[tokio::test]
    async fn guarded_withdrawal_blocks_removal_with_active_consumer() {
        use crate::Context;
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        ctx.provide(registry);
        let registry = ctx.get::<RegistryService>().unwrap();

        let provider_fid = registry.register(&ctx, FooPlugin, ()).expect("provider");
        let consumer_fid = register_foo_consumer(&ctx, &registry);
        assert!(matches!(
            registry.get_fiber(consumer_fid).unwrap().state(),
            FiberState::Active { .. }
        ));

        let key = (TypeId::of::<FooService>(), None);
        assert_eq!(registry.reliance_count(&key), 1);

        // Guarded withdrawal: removal must fail while the consumer is Active.
        let err = ctx.remove::<FooService>().expect_err("guard must block");
        assert!(
            err.to_string().contains("guarded withdrawal"),
            "error should mention guarded withdrawal, got {err}"
        );
        assert!(ctx.get::<FooService>().is_some(), "service stays provided");
        assert!(matches!(
            registry.get_fiber(provider_fid).unwrap().state(),
            FiberState::Active { .. }
        ));
    }

    #[tokio::test]
    async fn guarded_withdrawal_allows_after_consumer_gone() {
        use crate::Context;
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        ctx.provide(registry);
        let registry = ctx.get::<RegistryService>().unwrap();

        registry.register(&ctx, FooPlugin, ()).expect("provider");
        let consumer_fid = register_foo_consumer(&ctx, &registry);

        // Dispose the consumer: its effects are undone and it is no longer
        // Active, so reliance drops to zero and removal succeeds.
        let _ = registry.get_fiber(consumer_fid).unwrap().dispose().await;
        let key = (TypeId::of::<FooService>(), None);
        assert_eq!(registry.reliance_count(&key), 0);

        let removed = ctx.remove::<FooService>().expect("removal now allowed");
        assert_eq!(removed.unwrap().0, 1);
        assert!(ctx.get::<FooService>().is_none());
    }

    #[tokio::test]
    async fn internal_undo_bypasses_guard() {
        use crate::Context;
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        ctx.provide(registry);
        let registry = ctx.get::<RegistryService>().unwrap();

        registry.register(&ctx, FooPlugin, ()).expect("provider");
        register_foo_consumer(&ctx, &registry);
        let key = (TypeId::of::<FooService>(), None);
        assert_eq!(registry.reliance_count(&key), 1);

        // The forced path (used by fiber undo stacks / internal rollback)
        // removes even with an active consumer.
        let removed = ctx.remove_forced::<FooService>().expect("forced removal");
        assert!(removed.is_some());
        assert!(ctx.get::<FooService>().is_none());
    }

    #[tokio::test]
    async fn remove_without_registry_or_consumers_still_works() {
        use crate::Context;
        // No RegistryService on ctx: guard is inert, behavior as before.
        let ctx = Context::new_root();
        ctx.provide(FooService(2));
        let removed = ctx.remove::<FooService>().expect("unguarded removal");
        assert!(removed.is_some());
    }

    #[test]
    fn plugin_alias_behaves_like_register() {
        // Intentionally spelled out without the shared helper to keep the two
        // alias tests at distinct AST shapes; the helper path is exercised by
        // `successful_provide_retrievable_via_ctx_get` above.
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        let fid = registry
            .plugin(&ctx, FooPlugin, ())
            .expect("plugin alias ok");
        assert!(registry.get_fiber(fid).is_some());
        let svc = ctx
            .get::<FooService>()
            .expect("FooService should be present");
        assert_eq!(svc.0, 1);
        // Extra distinct check: FooService isolate should not have created BarService.
        assert!(ctx.get::<BarService>().is_none());
        assert_eq!(registry.len(), 1);
    }

    /// Dead-fiber pruning: a disposed fiber is dropped by `prune_disposed`
    /// (and by opportunistic sweeps on `len`), while a Failed fiber survives
    /// — inspectable by design.
    #[tokio::test]
    async fn prune_disposed_drops_disposed_but_keeps_failed() {
        let ctx = Context::new_root();
        let registry = RegistryService::new();

        let failed_fid = registry
            .register(&ctx, FailingPlugin, ())
            .expect_err("failing registration is rejected but tracked");
        let failed_fid = match failed_fid {
            CordisError::Configuration(_) => 1, // first allocation on a fresh registry
            other => panic!("unexpected error shape: {other:?}"),
        };
        let live_fid = registry
            .register(&ctx, FooPlugin, ())
            .expect("live registration");
        assert!(matches!(
            registry.get_fiber(failed_fid).unwrap().state(),
            FiberState::Failed { .. }
        ));

        // Force dispose the live fiber: its undos retract the service and the
        // `disposed` flag flips on.
        let _ = registry.get_fiber(live_fid).unwrap().dispose().await;
        assert!(registry.get_fiber(live_fid).unwrap().is_disposed());

        let pruned = registry.prune_disposed();
        assert_eq!(pruned, 1, "exactly the disposed fiber is pruned");
        assert!(
            registry.get_fiber(live_fid).is_none(),
            "disposed fiber must be gone after prune"
        );
        // Failed fiber SURVIVES the prune — inspectable-by-design.
        assert!(matches!(
            registry.get_fiber(failed_fid).unwrap().state(),
            FiberState::Failed { .. }
        ));
        // The live Active fiber is untouched bookkeeping too.
        let bar_fid = registry
            .register(&ctx, BarPlugin, ())
            .expect("bar registration");
        assert!(registry.get_fiber(bar_fid).is_some());

        // len() opportunistically re-prunes: dispose bar and a plain size
        // probe must clear it without an explicit prune call.
        let _ = registry.get_fiber(bar_fid).unwrap().dispose().await;
        assert_eq!(registry.len(), 1, "only the Failed fiber remains");
        assert!(registry.get_fiber(bar_fid).is_none());
    }

    /// Reactive Pending fibers survive `prune_disposed`: they are NOT
    /// disposed (no dispose ran), so the prune predicate keeps them tracked,
    /// and their provider slot stays reserved while they wait.
    #[tokio::test]
    async fn pending_fiber_survives_prune_disposed() {
        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        let reflect = ctx.get::<ReflectService>().unwrap();
        reflect.set_context(&ctx);
        let registry = RegistryService::new();
        ctx.provide(registry);
        let registry = ctx.get::<RegistryService>().unwrap();

        // Provider + consumer, consumer declares its inject reactively.
        let provider_fid = registry.register(&ctx, FooPlugin, ()).expect("provider");
        let fid = registry
            .register(&ctx, DependentPlugin, ())
            .expect("consumer registration");
        let fiber = registry.get_fiber(fid).unwrap();
        fiber.declare_inject::<FooService>();
        fiber.refresh(&ctx).await;
        assert!(matches!(fiber.state(), FiberState::Active { .. }));

        // Reactive dependency loss: the provider registration is retired
        // (disposed), which retracts the service and reactively notifies the
        // consumer.
        let _ = registry.get_fiber(provider_fid).unwrap().dispose().await;
        reflect.notify_with_ctx(TypeId::of::<FooService>(), &ctx).await;
        assert!(
            matches!(fiber.state(), FiberState::Pending),
            "consumer must rest Pending after dep loss, got {:?}",
            fiber.state()
        );

        // Prune drops the DISPOSED provider but keeps the Pending consumer:
        // the predicate is is_disposed(), and Pending fibers are not.
        let pruned = registry.prune_disposed();
        assert_eq!(pruned, 1, "exactly the disposed provider is pruned");
        assert!(
            matches!(fiber.state(), FiberState::Pending),
            "Pending fiber must survive prune"
        );
        assert!(registry.get_fiber(fid).is_some(), "still tracked");

        // Provider returns: the surviving Pending fiber reactivates.
        registry.register(&ctx, FooPlugin, ()).expect("provider back");
        reflect.notify_with_ctx(TypeId::of::<FooService>(), &ctx).await;
        assert!(
            matches!(fiber.state(), FiberState::Active { .. }),
            "reactivation after prune-pass, got {:?}",
            fiber.state()
        );
        assert_eq!(
            registry.len(),
            2,
            "both live fibers remain tracked through the cycle"
        );
    }

    /// Pruning a disposed provider clears its `provided` slot so the same key
    /// can register again without a stale duplicate-provider conflict.
    #[tokio::test]
    async fn prune_clears_provided_slot_for_fresh_registration() {
        let ctx = Context::new_root();
        let registry = RegistryService::new();
        let fid = registry
            .register(&ctx, FooPlugin, ())
            .expect("first registration");
        let _ = registry.get_fiber(fid).unwrap().dispose().await;

        // Without pruning, the stale entry does not block (the conflict check
        // already ignores non-active providers)...
        let fid2 = registry
            .register(&ctx, FooPlugin2, ())
            .expect("re-registration works even pre-prune");
        // ...but prune still removes both dead fibers from tracking: dispose
        // the stale original again (idempotent) and the fresh one too.
        let _ = registry.get_fiber(fid).unwrap().dispose().await;
        let _ = registry.get_fiber(fid2).unwrap().dispose().await;
        assert_eq!(registry.prune_disposed(), 2);
        assert!(registry.get_fiber(fid).is_none());
        assert!(registry.get_fiber(fid2).is_none());
        assert_eq!(registry.len(), 0);
    }

    // ------------------------------------------------------------------
    // Availability predicates (Service::check) at registration time
    // ------------------------------------------------------------------

    #[derive(Debug)]
    struct GatedService(bool);
    impl Service for GatedService {
        fn check(&self) -> bool {
            self.0
        }
    }

    struct GatedPlugin {
        ready: bool,
    }

    impl Plugin for GatedPlugin {
        type Config = ();
        type Provides = GatedService;

        fn apply(
            &self,
            _ctx: &Arc<Context>,
            _config: Self::Config,
        ) -> Result<Arc<Self::Provides>, CordisError> {
            Ok(Arc::new(GatedService(self.ready)))
        }
    }

    /// A plugin-produced service whose `Service::check` verdict is `false`
    /// rests its registration fiber as an inspectable `Failed` naming the
    /// rejection — never silently `Active`, never missing from tracking.
    /// Registration stays non-throwing (register-before-ready is a supported
    /// transient): a later ready provider of the same key converges the
    /// fiber back to Active on refresh.
    #[tokio::test]
    async fn availability_predicate_rejection_registers_failed() {
        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        if let Some(reflect) = ctx.get::<crate::ReflectService>() {
            reflect.set_context(&ctx);
        }
        let registry = RegistryService::new();

        let fid = registry
            .register(&ctx, GatedPlugin { ready: false }, ())
            .expect("predicate rejection must NOT fail registration");
        let fiber = registry.get_fiber(fid).expect("tracked");
        match fiber.state() {
            crate::FiberState::Failed { error } => {
                assert!(error
                    .as_deref()
                    .unwrap_or("")
                    .contains("availability predicate rejected service"));
            }
            other => panic!("expected Failed state, got {other:?}"),
        }
        // The unready value was never exposed to consumers.
        assert!(ctx.get::<GatedService>().is_none());
        // Convergence after a ready re-provision is covered by
        // predicate_passing_reregistration_activates_dependents below.
    }

    /// The reactive leg: after a rejected registration, registering a passing
    /// implementation of the same key activates dependents — mirroring the
    /// version_conformance shapes.
    #[tokio::test]
    async fn predicate_passing_reregistration_activates_dependents() {
        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        if let Some(reflect) = ctx.get::<crate::ReflectService>() {
            reflect.set_context(&ctx);
        }
        let registry = RegistryService::new();

        // Rejected first: registers fine but rests Failed with the key
        // unserved.
        let bad_fid = registry
            .register(&ctx, GatedPlugin { ready: false }, ())
            .expect("rejection is non-throwing");
        assert!(matches!(
            registry.get_fiber(bad_fid).unwrap().state(),
            crate::FiberState::Failed { .. }
        ));

        // Dependent declared against the gated TypeId while it is absent.
        struct GatedConsumer;
        impl Plugin for GatedConsumer {
            type Config = ();
            type Provides = DerivedProbe;

            fn apply(
                &self,
                _ctx: &Arc<Context>,
                _config: Self::Config,
            ) -> Result<Arc<Self::Provides>, CordisError> {
                Ok(Arc::new(DerivedProbe))
            }
        }

        #[derive(Debug)]
        struct DerivedProbe;
        impl Service for DerivedProbe {}

        let dep_fid = registry
            .register(&ctx, GatedConsumer, ())
            .expect("consumer registers even without its dependency");
        let dependent = registry.get_fiber(dep_fid).unwrap();
        dependent.declare_inject::<GatedService>();
        dependent.refresh(&ctx).await;
        assert!(
            matches!(dependent.state(), crate::FiberState::Inactive { .. }),
            "dependent must stay Inactive while the provider is rejected, got {:?}",
            dependent.state()
        );

        // A passing implementation of the same key now activates the
        // dependent through the reactive notify path.
        registry
            .register(&ctx, GatedPlugin { ready: true }, ())
            .expect("passing provider must register");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        dependent.refresh(&ctx).await;
        match dependent.state() {
            crate::FiberState::Active { .. } => {}
            other => {
                panic!("dependent should activate after a passing re-registration, got {other:?}")
            }
        }
        assert!(ctx.get::<GatedService>().is_some());
    }

    // ------------------------------------------------------------------
    // C2 readiness gates (ready_when): quiet Pending waiting, AND-composition,
    // external re-kick. Complement of the availability predicates above:
    // `Service::check` failures are LOUD (`Failed{error}`), a closed
    // readiness gate is QUIET (`Pending`, no error, factory never re-runs).
    // ------------------------------------------------------------------

    #[derive(Debug)]
    struct ReadinessProbe;
    impl Service for ReadinessProbe {}

    struct ReadinessPlugin;
    impl Plugin for ReadinessPlugin {
        type Config = ();
        type Provides = ReadinessProbe;

        fn apply(
            &self,
            _ctx: &Arc<Context>,
            _config: Self::Config,
        ) -> Result<Arc<Self::Provides>, CordisError> {
            Ok(Arc::new(ReadinessProbe))
        }
    }

    /// A registration whose `ready_when` gate starts closed rests its fiber
    /// as inspectable `Pending` (NOT Failed), keeps the produced service out
    /// of consumer reach, and flips to Active once the gate opens — without
    /// ever re-running the plugin factory.
    #[tokio::test]
    async fn ready_when_holds_pending_until_true_then_activates() {
        let ctx = Context::new_root();
        let registry = RegistryService::new();

        let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gate_flag = open.clone();
        let fid = registry
            .register_with_readiness(
                &ctx,
                ReadinessPlugin,
                (),
                ReadinessBarrier::new(move |_ctx| {
                    gate_flag.load(std::sync::atomic::Ordering::Acquire)
                }),
            )
            .expect("gated registration is non-throwing");

        // The freshly-installed gate decides the resting state on the
        // registration's own re-entry pass.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let fiber = registry.get_fiber(fid).expect("tracked");
        assert!(
            matches!(fiber.state(), FiberState::Pending),
            "closed gate must rest Pending, got {:?}",
            fiber.state()
        );
        assert!(
            ctx.get::<ReadinessProbe>().is_none(),
            "strict get refuses values owned by non-Active fibers"
        );

        // Open the gate and re-kick through the normal lifecycle entry point.
        open.store(true, std::sync::atomic::Ordering::Release);
        fiber.refresh(&ctx).await;
        match fiber.state() {
            FiberState::Active { .. } => {}
            other => panic!("open gate must activate, got {other:?}"),
        }
        assert!(ctx.get::<ReadinessProbe>().is_some(), "now served");
    }

    /// AND-composition via [`with_readiness`]: the combined barrier is ready
    /// only when EVERY operand reports ready; opening one half while the
    /// other stays closed keeps the fiber waiting.
    #[tokio::test]
    async fn readiness_composes_and_semantics() {
        let ctx = Context::new_root();
        let registry = RegistryService::new();

        let a_open = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let b_open = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let combined = with_readiness([
            {
                let flag = a_open.clone();
                ReadinessBarrier::new(move |_ctx| {
                    flag.load(std::sync::atomic::Ordering::Acquire)
                })
            },
            {
                let flag = b_open.clone();
                ReadinessBarrier::new(move |_ctx| {
                    flag.load(std::sync::atomic::Ordering::Acquire)
                })
            },
        ]);
        // Direct evaluation first: both closed / one open / both open.
        assert!(!combined.is_ready(&ctx), "both closed must not be ready");
        a_open.store(true, std::sync::atomic::Ordering::Release);
        assert!(
            !combined.is_ready(&ctx),
            "AND semantics: one open half is not enough"
        );
        b_open.store(true, std::sync::atomic::Ordering::Release);
        assert!(combined.is_ready(&ctx), "both open must be ready");
        // The empty composition is vacuously ready.
        assert!(with_readiness([]).is_ready(&ctx));

        // Register while only HALF the composed gate is open: the fiber must
        // keep waiting Pending (AND semantics at rest), then activate once
        // the second half opens.
        b_open.store(false, std::sync::atomic::Ordering::Release);
        a_open.store(true, std::sync::atomic::Ordering::Release);

        let fid = registry
            .register_with_readiness(&ctx, ReadinessPlugin, (), combined)
            .expect("composed-gate registration");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let fiber = registry.get_fiber(fid).expect("tracked");
        assert!(
            matches!(fiber.state(), FiberState::Pending),
            "one-open-half must still wait Pending, got {:?}",
            fiber.state()
        );

        // Second half opens: the composed gate goes ready and the fiber
        // activates on its next lifecycle pass.
        b_open.store(true, std::sync::atomic::Ordering::Release);
        fiber.refresh(&ctx).await;
        match fiber.state() {
            FiberState::Active { .. } => {}
            other => panic!("fully-open AND gate must activate, got {other:?}"),
        }
    }

    /// Re-kick wiring: an EXTERNAL settle (another managed fiber providing /
    /// withdrawing) fans out through the round-5 observer notify path and
    /// re-evaluates the gated fiber — it activates without any direct call
    /// to refresh on the gated fiber itself.
    #[tokio::test]
    async fn external_rekick_reactivates_waiting_fiber() {
        let ctx = Context::new_root();
        ctx.provide(ReflectService::new());
        if let Some(reflect) = ctx.get::<crate::ReflectService>() {
            reflect.set_context(&ctx);
        }
        let registry = RegistryService::new();
        ctx.provide(registry);
        let registry = ctx.get::<RegistryService>().unwrap();

        // Gate observes a plain context fact: whether Dependency is provided.
        // `watching` declares the settle source so external provides /
        // withdrawals of Dependency re-kick this fiber through Reflect.
        let fid = registry
            .register_with_readiness(
                &ctx,
                ReadinessPlugin,
                (),
                ReadinessBarrier::new(|ctx: &Arc<Context>| ctx.get::<Dependency>().is_some())
                    .watching([TypeId::of::<Dependency>()]),
            )
            .expect("fact-gated registration");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let fiber = registry.get_fiber(fid).expect("tracked");
        assert!(matches!(fiber.state(), FiberState::Pending));

        // External settle: another fiber provides Dependency. The provide
        // notifies the ReflectService fan-out (round-5 observer path), which
        // BFS-refreshes dependents — including our gated fiber.
        let dep_fid = registry
            .register(&ctx, BarPlugin, ())
            .expect("dependency provider registers");
        assert!(dep_fid > 0);
        ctx.provide(Dependency);
        if let Some(reflect) = ctx.get::<crate::ReflectService>() {
            reflect.notify_with_ctx(TypeId::of::<Dependency>(), &ctx).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        match fiber.state() {
            FiberState::Active { .. } => {}
            other => panic!(
                "external re-kick must reactivate the waiting fiber, got {other:?}"
            ),
        }
        assert!(ctx.get::<ReadinessProbe>().is_some());

        // Withdrawal settles too: the fact flips, the next re-kick rests the
        // fiber back to Pending — never Failed — proving bidirectional
        // complementarity with loud predicate failures.
        let _removed = ctx.remove::<Dependency>();
        if let Some(reflect) = ctx.get::<crate::ReflectService>() {
            reflect.notify_with_ctx(TypeId::of::<Dependency>(), &ctx).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            matches!(fiber.state(), FiberState::Pending),
            "gate closing again must rest Pending quietly, got {:?}",
            fiber.state()
        );
    }
}

#[cfg(test)]
mod config_waterfall_tests {
    use super::*;
    use crate::events::{EventsService, INTERNAL_CONFIG_EVENT};
    use crate::{Context, FiberState};

    /// Captures the config its factory actually received.
    struct CapturePlugin;
    #[derive(Debug)]
    struct CapturedConfig(pub serde_json::Value);
    impl Service for CapturedConfig {}
    impl Plugin for CapturePlugin {
        type Config = serde_json::Value;
        type Provides = CapturedConfig;
        fn apply(
            &self,
            _ctx: &Arc<Context>,
            cfg: Self::Config,
        ) -> Result<Arc<Self::Provides>, CordisError> {
            Ok(Arc::new(CapturedConfig(cfg)))
        }
    }

    /// C1 `internal/config` covers the ACTIVATION path: the very first runner
    /// pass at registration time must apply the intercepted (effective)
    /// config, not the raw one. A multi-thread runtime is required so the
    /// synchronous bridge can park the worker.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_waterfall_covers_activation_path() {
        let ctx = Context::new_root();
        let events = Arc::new(EventsService::new());
        ctx.provide_arc(events.clone());
        let _gate = events.on(INTERNAL_CONFIG_EVENT.into(), |raw| async move {
            let mut effective = raw;
            if let Some(obj) = effective.as_object_mut() {
                obj.insert("rewritten_at_activation".into(), serde_json::json!(true));
            }
            Ok(effective)
        });

        let registry = RegistryService::new();
        let fid = registry
            .register(
                &ctx,
                CapturePlugin,
                serde_json::json!({ "model": "base" }),
            )
            .expect("registration with an intercept-config listener");
        assert!(matches!(
            registry.get_fiber(fid).unwrap().state(),
            FiberState::Active { .. }
        ));
        let captured = ctx.get::<CapturedConfig>().expect("provider active");
        assert_eq!(captured.0["model"], "base");
        assert_eq!(
            captured.0["rewritten_at_activation"], true,
            "activation pass must consume the EFFECTIVE config"
        );
    }
}
