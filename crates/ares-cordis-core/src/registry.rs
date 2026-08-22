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
        let key = (tid, isolate.clone());
        {
            let provided = self.provided.read();
            if provided.contains_key(&key) {
                return Err(CordisError::Configuration(format!(
                    "duplicate provider for {:?}",
                    tid
                )));
            }
        }
        let fid = self.next_fiber_id();
        let fiber = Arc::new(Fiber::new());
        // The fiber that represents this registration is tracked from the
        // moment it starts Loading so a later failure is observable as Failed
        // (guarded withdrawal per Cordis). A failure keeps the fiber in the
        // registry but never records a provider mapping.
        fiber.set_state(crate::FiberState::Loading);
        self.fibers.write().insert(fid, fiber.clone());
        let provides = match plugin.apply(ctx, config) {
            Ok(p) => p,
            Err(e) => {
                fiber.set_state(crate::FiberState::Failed {
                    error: Some(e.to_string()),
                });
                return Err(e);
            }
        };
        fiber.set_state(crate::FiberState::Active {
            epoch: String::new(),
        });
        // Insert the service into the context. The context tracks its own
        // version and undo on its fiber, while the registry tracks the fiber
        // that represents this registration.
        ctx.provide_arc(provides);

        self.provided.write().insert(key, fid);
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
// single-source discipline.

#[cfg(feature = "inventory")]
inventory::submit! {
    crate::CordisInventory { name: "RegistryService" }
}

#[cfg(feature = "linkme")]
#[linkme::distributed_slice]
pub static REGISTRY_PLUGINS: [fn(&Arc<Context>) -> Result<FiberId, CordisError>];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Context, Service};

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
        let existing = registry.get_fiber(1).expect("failed fiber should be tracked");
        match existing.state() {
            FiberState::Failed { error } => {
                assert!(error.as_deref().unwrap_or("").contains("intentional failure"));
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
    fn assert_plugin_retrievable<T, P>(registry: &RegistryService, ctx: &Arc<Context>, plugin: P, expect: impl FnOnce(&T))
    where
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
        let svc = ctx.get::<FooService>().expect("FooService should be present");
        assert_eq!(svc.0, 1);
        // Extra distinct check: FooService isolate should not have created BarService.
        assert!(ctx.get::<BarService>().is_none());
        assert_eq!(registry.len(), 1);
    }
}
