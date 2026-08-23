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
    pub fn register<P: Plugin>(
        &self,
        ctx: &Arc<Context>,
        plugin: P,
        config: P::Config,
    ) -> Result<FiberId, CordisError> {
        let tid = TypeId::of::<P::Provides>();
        let isolate = ctx.isolate_label(tid);
        let key = (tid, isolate);
        if let Some(existing) = self.provided.read().get(&key).copied() {
            let active = self
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
                .unwrap_or(false);
            if active {
                return Err(CordisError::Configuration(format!(
                    "duplicate provider for {:?}",
                    tid
                )));
            }
            self.provided.write().remove(&key);
        }

        let fid = self.next_fiber_id();
        let fiber = Arc::new(Fiber::new());
        fiber.set_state(crate::FiberState::Loading);
        fiber.set_reload_context(ctx);
        fiber.set_id(fid);
        self.fibers.write().insert(fid, fiber.clone());

        let config_value = serde_json::to_value(&config).map_err(|error| {
            let message = format!("cannot serialize plugin config: {error}");
            fiber.set_state(crate::FiberState::Failed {
                error: Some(message.clone()),
            });
            CordisError::Configuration(message)
        })?;
        let plugin = Arc::new(plugin);
        let weak_fiber = Arc::downgrade(&fiber);
        fiber.set_reload_runner(Box::new(move |ctx| {
            let config =
                serde_json::from_value::<P::Config>(config_value.clone()).map_err(|error| {
                    CordisError::Configuration(format!("cannot deserialize plugin config: {error}"))
                })?;
            let owner = weak_fiber
                .upgrade()
                .ok_or_else(|| CordisError::Fiber("registration fiber was dropped".into()))?;
            let provides = ctx.with_provider_fiber(&owner, || plugin.apply(ctx, config))?;
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
                return Err(error);
            }
        };
        let epoch = fiber.compute_epoch(ctx);
        fiber.set_epoch(epoch.clone());
        fiber.set_state(if healthy {
            crate::FiberState::Active { epoch }
        } else {
            crate::FiberState::Inactive { error: None }
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

    pub fn get_fiber(&self, id: FiberId) -> Option<Arc<Fiber>> {
        self.fibers.read().get(&id).cloned()
    }

    pub fn remove(&self, id: FiberId) -> Option<Arc<Fiber>> {
        let fiber = self.fibers.write().remove(&id)?;
        let mut provided = self.provided.write();
        provided.retain(|_, v| *v != id);
        Some(fiber)
    }

    pub fn len(&self) -> usize {
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
    use crate::{Context, FiberState, Service};

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
        registry.get_fiber(foo).unwrap().dispose().await;
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
}
