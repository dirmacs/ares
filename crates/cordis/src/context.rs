use parking_lot::RwLock;
use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

thread_local! {
    static ACTIVE_PROVIDER_FIBERS: RefCell<Vec<(usize, Arc<Fiber>)>> = const { RefCell::new(Vec::new()) };
}

use crate::fiber::{Fiber, FiberState};
use crate::registry::{Plugin, RegistryService};
use crate::service::{CordisError, Service};
use crate::{FiberId, ReflectService, Symbol};

pub(crate) static NEXT_FIBER_ID: AtomicUsize = AtomicUsize::new(1);

pub struct Context {
    store: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    isolate: RwLock<HashMap<TypeId, Symbol>>,
    intercept: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    versions: RwLock<HashMap<TypeId, u64>>,
    /// Semantic peer-dependency versions declared alongside a value by
    /// [`Context::provide_versioned`]. Legacy `provide` paths keep this map
    /// empty, so an absent entry reads as provider version 0.
    provided_versions: RwLock<HashMap<TypeId, u64>>,
    // Providers installed by a registration fiber are hidden while that fiber
    // is inactive, reloading, or failed. Direct `provide` values remain
    // permissive for existing root/test APIs.
    owners: RwLock<HashMap<TypeId, Weak<Fiber>>>,
    fiber: Arc<Fiber>,
    parent: Option<Arc<Context>>,
    root: Weak<Context>,
}

impl Context {
    pub(crate) fn with_provider_fiber<R>(
        self: &Arc<Self>,
        fiber: &Arc<Fiber>,
        f: impl FnOnce() -> R,
    ) -> R {
        let key = Arc::as_ptr(self) as usize;
        ACTIVE_PROVIDER_FIBERS.with(|stack| stack.borrow_mut().push((key, fiber.clone())));
        struct Scope;
        impl Drop for Scope {
            fn drop(&mut self) {
                ACTIVE_PROVIDER_FIBERS.with(|stack| {
                    let _ = stack.borrow_mut().pop();
                });
            }
        }
        let _scope = Scope;
        f()
    }

    fn active_provider_fiber(&self) -> Option<Arc<Fiber>> {
        let key = self as *const Context as usize;
        ACTIVE_PROVIDER_FIBERS.with(|stack| {
            stack
                .borrow()
                .iter()
                .rev()
                .find(|(context, _)| *context == key)
                .map(|(_, fiber)| fiber.clone())
        })
    }
    pub fn new_root() -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            store: RwLock::new(HashMap::new()),
            isolate: RwLock::new(HashMap::new()),
            intercept: RwLock::new(HashMap::new()),
            versions: RwLock::new(HashMap::new()),
            provided_versions: RwLock::new(HashMap::new()),
            owners: RwLock::new(HashMap::new()),
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
            provided_versions: RwLock::new(HashMap::new()),
            owners: RwLock::new(HashMap::new()),
            fiber: Arc::new(Fiber::new()),
            parent: Some(self.clone()),
            root: self.root.clone(),
        })
    }

    pub fn isolate_type(self: &Arc<Self>, tid: TypeId, label: impl Into<Symbol>) -> Arc<Self> {
        let mut parent_isolate = self.isolate.read().clone();
        parent_isolate.insert(tid, label.into());
        Arc::new(Self {
            store: RwLock::new(HashMap::new()),
            isolate: RwLock::new(parent_isolate),
            intercept: RwLock::new(HashMap::new()),
            versions: RwLock::new(HashMap::new()),
            provided_versions: RwLock::new(HashMap::new()),
            owners: RwLock::new(HashMap::new()),
            fiber: Arc::new(Fiber::new()),
            parent: Some(self.clone()),
            root: self.root.clone(),
        })
    }

    pub fn isolate<T: Service>(self: &Arc<Self>, label: impl Into<Symbol>) -> Arc<Self> {
        self.isolate_type(TypeId::of::<T>(), label)
    }

    pub fn intercept<T: Service>(self: &Arc<Self>, val: T) -> Arc<Self> {
        let child = self.extend();
        let tid = TypeId::of::<T>();
        let any: Arc<dyn Any + Send + Sync> = Arc::new(val);
        child.intercept.write().insert(tid, any);
        // bump version for intercept as well? Not needed for epoch but keep
        child
    }

    /// Semantic peer-dependency versioning scheme (paper §open-problems,
    /// peer dependencies).
    ///
    /// A provider's version is a plain `u64`. The **major** component lives in
    /// the high bits: `major(v) = v / 100_000`, and the **minimum compatible
    /// floor** is the remainder `v % 100_000` within that major. An inject
    /// constrained with `requirement = M * 100_000 + f` is satisfied by a
    /// provider of version `p` if and only if
    ///
    /// * the provider exists and is available, and
    /// * `major(p) == major(requirement)` (same-major compatibility — peer
    ///   dependencies never bind across a breaking boundary), and
    /// * `p >= requirement` (the provider is at least the requested floor;
    ///   under equal majors this is exactly "remainder >= floor").
    ///
    /// Any mismatch leaves the inject **unsatisfied**: the dependent fiber
    /// goes, and stays, `Inactive` rather than silently binding a wrong
    /// version. Providers installed through legacy [`Self::provide`] carry
    /// version 0, so they satisfy only unconstrained injects; migrate them to
    /// [`Self::provide_versioned`] to opt into constraint matching.
    pub const VERSION_MAJOR_SCALE: u64 = 100_000;

    /// Provide `value` under `T` together with a semantic peer-dependency
    /// version. See the [`Self::VERSION_MAJOR_SCALE`] documentation for the
    /// exact satisfaction scheme. Ownership/undo semantics are identical to
    /// [`Self::provide`].
    pub fn provide_versioned<T: Any + Send + Sync>(
        self: &Arc<Self>,
        value: T,
        version: u64,
    ) -> Arc<T> {
        let owner = self.active_provider_fiber();
        self.provide_impl(Arc::new(value), owner.as_ref(), Some(version))
    }

    /// The semantic peer-dependency version recorded for `tid`, walking the
    /// parent chain like [`Self::get_version`]. Returns 0 when no value was
    /// provided or when it was installed through an unversioned path.
    ///
    /// Distinct from [`Self::get_version`], which counts structural store
    /// mutations of `tid` and drives epoch strings; this reads the declared
    /// compatibility contract used by version-constrained injects.
    pub fn provider_version(&self, tid: TypeId) -> u64 {
        if let Some(v) = self.provided_versions.read().get(&tid) {
            return *v;
        }
        if let Some(parent) = &self.parent {
            return parent.provider_version(tid);
        }
        0
    }

    // Direct providers are owned by this context's fiber for compatibility.
    pub fn provide<T: Service>(self: &Arc<Self>, svc: T) -> Arc<T> {
        let owner = self.active_provider_fiber();
        self.provide_impl(Arc::new(svc), owner.as_ref(), None)
    }

    /// Install a provider and record its undo on an explicit registration
    /// fiber. This is used by RegistryService so disposing one registration
    /// cannot remove another registration's service.
    pub(crate) fn provide_on_fiber<T: Service>(
        self: &Arc<Self>,
        svc: Arc<T>,
        owner: &Arc<Fiber>,
    ) -> Arc<T> {
        self.provide_impl(svc, Some(owner), None)
    }

    /// Install a service value in this context's store.
    ///
    /// `semantic_version` carries the peer-dependency contract from
    /// [`Context::provide_versioned`]: `Some(v)` records `v` in the
    /// [`Self::provided_versions`] map (visible to [`Self::provider_version`]
    /// and to inject satisfaction checks), `None` clears any entry so legacy
    /// `provide` paths read as provider version 0. This is independent of the
    /// structural mutation counter in `versions`, which keeps ticking on every
    /// store change.
    fn provide_impl<T: Any + Send + Sync>(
        self: &Arc<Self>,
        svc: Arc<T>,
        owner: Option<&Arc<Fiber>>,
        semantic_version: Option<u64>,
    ) -> Arc<T> {
        let tid = TypeId::of::<T>();
        let any: Arc<dyn Any + Send + Sync> = svc.clone();
        let prev = self.store.write().insert(tid, any.clone());
        if tid == TypeId::of::<ReflectService>() {
            if let Ok(reflect) = any.downcast::<ReflectService>() {
                reflect.set_context(self);
            }
        }
        let prev_owner = if let Some(owner) = owner {
            self.owners.write().insert(tid, Arc::downgrade(owner))
        } else {
            self.owners.write().remove(&tid)
        };
        {
            let mut versions = self.versions.write();
            *versions.entry(tid).or_insert(0) += 1;
        }
        let prev_semantic = match semantic_version {
            Some(v) => self.provided_versions.write().insert(tid, v),
            None => self.provided_versions.write().remove(&tid),
        };
        if let Some(reflect) = self.get::<ReflectService>() {
            reflect.notify(tid);
        }
        let weak = Arc::downgrade(self);
        let undo: Box<dyn FnOnce() + Send> = Box::new(move || {
            if let Some(ctx) = weak.upgrade() {
                // Release all context write guards before looking up ReflectService.
                // Context get reads the store; calling it while the store write lock is held
                // deadlocks parking_lot's non-reentrant RwLock during disposal.
                {
                    let mut store = ctx.store.write();
                    if let Some(prev_any) = prev {
                        store.insert(tid, prev_any);
                    } else {
                        store.remove(&tid);
                    }
                    let mut owners = ctx.owners.write();
                    if let Some(previous) = prev_owner {
                        owners.insert(tid, previous);
                    } else {
                        owners.remove(&tid);
                    }
                    let mut versions = ctx.versions.write();
                    if let Some(v) = versions.get_mut(&tid) {
                        *v = v.saturating_sub(1);
                        if *v == 0 {
                            versions.remove(&tid);
                        }
                    }
                    // Restore the semantic peer-version entry the provide
                    // replaced (or clear it when none existed), mirroring the
                    // store/owners LIFO discipline.
                    match prev_semantic {
                        Some(v) => {
                            ctx.provided_versions.write().insert(tid, v);
                        }
                        None => {
                            ctx.provided_versions.write().remove(&tid);
                        }
                    }
                }
                if let Some(reflect) = ctx.get::<ReflectService>() {
                    reflect.notify(tid);
                }
            }
        });
        owner
            .cloned()
            .unwrap_or_else(|| self.fiber.clone())
            .push_undo(undo);
        svc
    }

    /// Remove a service from the store and trigger deactivation cascade.
    ///
    /// Guarded withdrawal: when a [`RegistryService`] is present and active
    /// consumer fibers still resolve `T` in this isolate realm, removal is
    /// refused with a `guarded withdrawal` configuration error instead of
    /// pulling the dependency out from under them. Internal rollback paths
    /// (fiber undo stacks) bypass the guard via [`Self::remove_forced`].
    /// Pushes the inverse (re-provide) onto the fiber's accumulator for LIFO
    /// reversal once the guard permits the removal.
    pub fn remove<T: Service>(self: &Arc<Self>) -> Result<Option<Arc<T>>, CordisError> {
        let tid = TypeId::of::<T>();
        if self.store.read().contains_key(&tid) {
            if let Some(registry) = self.get::<RegistryService>() {
                let key = (tid, self.isolate_label(tid));
                let consumers = registry.reliance_count(&key);
                if consumers > 0 {
                    return Err(CordisError::Configuration(format!(
                        "guarded withdrawal: {consumers} active consumer(s) still rely on {}",
                        std::any::type_name::<T>()
                    )));
                }
            }
        }
        Self::remove_forced::<T>(self)
    }

    /// Unconditional removal -- the rollback primitive behind [`Self::remove`].
    /// Internal undo paths must never be blocked by the guarded-withdrawal
    /// check, so they call this directly.
    pub(crate) fn remove_forced<T: Service>(
        self: &Arc<Self>,
    ) -> Result<Option<Arc<T>>, CordisError> {
        let tid = TypeId::of::<T>();
        let removed = {
            let mut store = self.store.write();
            store.remove(&tid)
        };
        if let Some(any) = removed {
            let previous_owner = self.owners.write().remove(&tid);
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
            let fiber = self
                .active_provider_fiber()
                .unwrap_or_else(|| self.fiber.clone());
            let any_clone = any.clone();
            let undo: Box<dyn FnOnce() + Send> = Box::new(move || {
                if let Some(ctx) = weak.upgrade() {
                    ctx.store.write().insert(tid, any_clone);
                    if let Some(owner) = previous_owner {
                        ctx.owners.write().insert(tid, owner);
                    }
                    let mut versions = ctx.versions.write();
                    let e = versions.entry(tid).or_insert(0);
                    *e += 1;
                }
            });
            fiber.push_undo(undo);
            // Downcast to the concrete type
            Ok(any.downcast::<T>().ok())
        } else {
            Ok(None)
        }
    }

    /// Install an already-built service under a concrete `TypeId` without
    /// registering it with the [`RegistryService`].
    ///
    /// Used by the loader's verified hot-swap trial: the candidate fiber is
    /// applied out-of-band first so a failing factory cannot kill the live
    /// provider. Returns `Err(tid)` when the slot is already occupied.
    pub(crate) fn provide_untyped(
        self: &Arc<Self>,
        tid: TypeId,
        any: Arc<dyn Any + Send + Sync>,
    ) -> Result<(), TypeId> {
        let mut store = self.store.write();
        if store.contains_key(&tid) {
            return Err(tid);
        }
        if tid == TypeId::of::<ReflectService>() {
            if let Ok(reflect) = any.clone().downcast::<ReflectService>() {
                reflect.set_context(self);
            }
        }
        store.insert(tid, any);
        Ok(())
    }

    /// Take the raw value stored for `tid` without notifying dependents or
    /// touching versions/owners. Companion to [`Self::provide_untyped`] for
    /// the verified hot-swap trial teardown.
    pub(crate) fn take_untyped(&self, tid: TypeId) -> Option<Arc<dyn Any + Send + Sync>> {
        self.store.write().remove(&tid)
    }

    /// Untyped availability probe mirroring `get::<T>()` semantics without a
    /// generic parameter (the trial's `Provides` type is erased).
    pub(crate) fn get_untyped(&self, tid: TypeId) -> Option<Arc<dyn Any + Send + Sync>> {
        self.store.read().get(&tid).cloned()
    }

    /// Install an untyped intercept override without forking a child context.
    /// Companion to [`Self::bind_intercept`] for erased service values.
    pub(crate) fn bind_intercept_untyped(&self, tid: TypeId, any: Arc<dyn Any + Send + Sync>) {
        self.intercept.write().insert(tid, any);
    }

    /// Read an intercept override without removing it.
    pub(crate) fn peek_intercept_untyped(&self, tid: TypeId) -> Option<Arc<dyn Any + Send + Sync>> {
        self.intercept.read().get(&tid).cloned()
    }

    /// Drop an intercept override.
    pub(crate) fn remove_intercept_untyped(&self, tid: TypeId) {
        self.intercept.write().remove(&tid);
    }

    /// Relaxed read: like [`Self::get`], but a locally-owned provider whose
    /// owner fiber rests in a TRANSITIONING state (`Loading`, `Reloading`,
    /// `Unloading`, or reactive `Pending`) still resolves. Strict [`Self::get`]
    /// refuses those so consumers never observe mid-transition values;
    /// lifecycle/observer code (and tests) use this to inspect the value that
    /// is about to serve or was just retracted during transitions.
    ///
    /// Terminal resting states (`Failed`, disposed) and missing owners stay
    /// refused exactly as in [`Self::get`].
    pub fn get_relaxed<T: Service>(&self) -> Option<Arc<T>> {
        let tid = TypeId::of::<T>();
        if self.isolate_label(tid).is_none() {
            if let Some(any) = self.intercept.read().get(&tid) {
                if let Ok(arc) = any.clone().downcast::<T>() {
                    return Some(arc);
                }
            }
        }
        if let Some(any) = self.store.read().get(&tid) {
            let transitioning = self
                .owners
                .read()
                .get(&tid)
                .and_then(Weak::upgrade)
                .map(|fiber| {
                    matches!(
                        fiber.state(),
                        FiberState::Active { .. }
                            | FiberState::Loading
                            | FiberState::Reloading
                            | FiberState::Unloading { .. }
                            | FiberState::Pending
                    )
                })
                .unwrap_or(true);
            // Disposed fibers stay refused even in relaxed mode: disposal
            // already ran its undos, so the value is logically gone.
            if transitioning && !self.disposed_owner(tid) {
                if let Ok(arc) = any.clone().downcast::<T>() {
                    return Some(arc);
                }
            }
            return None;
        }
        if self.isolate.read().contains_key(&tid) {
            return self.parent.as_ref().and_then(|parent| {
                if parent.isolate_label(tid) == self.isolate_label(tid) {
                    parent.get_relaxed::<T>()
                } else {
                    None
                }
            });
        }
        self.parent.as_ref().and_then(|parent| parent.get_relaxed::<T>())
    }

    /// True when the owner fiber of `tid` has been disposed.
    fn disposed_owner(&self, tid: TypeId) -> bool {
        self.owners
            .read()
            .get(&tid)
            .and_then(Weak::upgrade)
            .map(|fiber| fiber.is_disposed())
            .unwrap_or(false)
    }

    pub fn get<T: Service>(&self) -> Option<Arc<T>> {
        let tid = TypeId::of::<T>();
        // Isolate-labeled TypeIds resolve from store / isolate parent walk.
        // Unlabeled TypeIds still let intercept win (request-scoped override).
        if self.isolate_label(tid).is_none() {
            if let Some(any) = self.intercept.read().get(&tid) {
                if let Ok(arc) = any.clone().downcast::<T>() {
                    return Some(arc);
                }
            }
        }
        let mut local_provider = false;
        if let Some(any) = self.store.read().get(&tid) {
            local_provider = true;
            let active = self
                .owners
                .read()
                .get(&tid)
                .and_then(Weak::upgrade)
                .map(|fiber| matches!(fiber.state(), FiberState::Active { .. }))
                .unwrap_or(true);
            if active {
                if let Ok(arc) = any.clone().downcast::<T>() {
                    if arc.check() {
                        return Some(arc);
                    }
                }
            }
        }
        if local_provider {
            return None;
        }
        // An isolate label is a realm boundary. A provider from an unlabeled
        // parent must not leak into a labeled child, nor may another label
        // satisfy this lookup. Unrelated service types still walk normally.
        if self.isolate.read().contains_key(&tid) {
            return self.parent.as_ref().and_then(|parent| {
                if parent.isolate_label(tid) == self.isolate_label(tid) {
                    parent.get::<T>()
                } else {
                    None
                }
            });
        }
        self.parent.as_ref().and_then(|parent| parent.get::<T>())
    }

    /// The single-source-discipline refusal for a TypeId that is already
    /// provided in the same isolate realm. Uses the structured
    /// [`CordisError::DuplicateProvider`] variant; its Display keeps the
    /// `duplicate provider` phrase that tests and docs assert on.
    fn duplicate_provider_error(tid: TypeId) -> CordisError {
        CordisError::DuplicateProvider {
            name: format!("{tid:?}"),
            owner: "context".to_string(),
        }
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

    pub(crate) fn is_available(&self, tid: TypeId) -> bool {
        if self.isolate_label(tid).is_none() && self.intercept.read().contains_key(&tid) {
            return true;
        }
        if self.store.read().contains_key(&tid) {
            return self
                .owners
                .read()
                .get(&tid)
                .and_then(Weak::upgrade)
                .map(|fiber| matches!(fiber.state(), FiberState::Active { .. }))
                .unwrap_or(true);
        }
        if self.isolate.read().contains_key(&tid) {
            return self.parent.as_ref().is_some_and(|parent| {
                parent.isolate_label(tid) == self.isolate_label(tid) && parent.is_available(tid)
            });
        }
        self.parent
            .as_ref()
            .is_some_and(|parent| parent.is_available(tid))
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
                    let active = self
                        .owners
                        .read()
                        .get(&tid)
                        .and_then(Weak::upgrade)
                        .map(|fiber| matches!(fiber.state(), FiberState::Active { .. }))
                        .unwrap_or(true);
                    if active {
                        if let Ok(arc) = any.clone().downcast::<T>() {
                            if arc.check() {
                                return Some(arc);
                            }
                        }
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
    ///
    /// If [`ReflectService`] is on the context, wait on its `TypeId` notifier
    /// (`ensure_notifier` + `changed`) so `provide` → `notify` unblocks without
    /// polling. If the sender is dropped, or ReflectService is absent, fall
    /// through to a 5ms poll loop so tests without Reflect still complete.
    pub async fn inject<T: Service>(self: &Arc<Self>) -> Arc<T> {
        if let Some(value) = self.get::<T>() {
            return value;
        }
        if let Some(reflect) = self.get::<ReflectService>() {
            let mut rx = reflect.ensure_notifier(TypeId::of::<T>());
            loop {
                if let Some(value) = self.get::<T>() {
                    return value;
                }
                if rx.changed().await.is_err() {
                    break;
                }
            }
        }
        loop {
            if let Some(value) = self.get::<T>() {
                return value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    pub fn provide_arc<T: Service>(self: &Arc<Self>, svc: Arc<T>) -> Arc<T> {
        let owner = self.active_provider_fiber();
        self.provide_impl(svc, owner.as_ref(), None)
    }

    pub fn fiber(&self) -> Arc<Fiber> {
        self.fiber.clone()
    }

    // Snapshot for temporal test: capture store length + versions
    pub fn snapshot_len(&self) -> usize {
        self.store.read().len()
    }

    pub async fn plugin<S: Service>(self: &Arc<Self>, svc: S) -> Result<FiberId, CordisError> {
        let tid = TypeId::of::<S>();
        if self.store.read().contains_key(&tid) {
            return Err(Self::duplicate_provider_error(tid));
        }
        let fiber = self.fiber.clone();
        fiber.set_state(FiberState::Loading);
        let disposable = match svc.init(self).await {
            Ok(d) => d,
            Err(e) => {
                fiber.set_state(FiberState::Failed {
                    error: Some(e.to_string()),
                });
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
            fiber.set_epoch(epoch.clone());
            fiber.set_state(FiberState::Active { epoch });
        } else {
            fiber.set_state(FiberState::Inactive { error: None });
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

    pub async fn plugin_with<P: Plugin>(
        self: &Arc<Self>,
        plugin: P,
        config: P::Config,
    ) -> Result<FiberId, CordisError> {
        if let Some(registry) = self.get::<RegistryService>() {
            return registry.plugin(self, plugin, config);
        }
        let tid = TypeId::of::<P::Provides>();
        if self.store.read().contains_key(&tid) {
            return Err(Self::duplicate_provider_error(tid));
        }
        self.fiber.set_state(FiberState::Loading);
        let provides = match plugin.apply(self, config) {
            Ok(p) => p,
            Err(e) => {
                self.fiber.set_state(FiberState::Failed {
                    error: Some(e.to_string()),
                });
                return Err(e);
            }
        };
        self.provide_arc(provides);
        let epoch = self.fiber.compute_epoch(self);
        self.fiber.set_epoch(epoch.clone());
        self.fiber.set_state(FiberState::Active { epoch });
        let fid = NEXT_FIBER_ID.fetch_add(1, Ordering::SeqCst) as u64;
        Ok(fid)
    }
}

#[cfg(test)]
mod relaxed_tests {
    use super::*;
    use crate::fiber::FiberState;

    /// `get_relaxed` reads a locally-owned value while its owner fiber sits
    /// mid-transition (`Loading`/`Reloading`/`Unloading`/`Pending`), where
    /// strict [`Context::get`] still refuses; terminal/disposed owners stay
    /// refused even in relaxed mode.
    #[tokio::test]
    async fn relaxed_read_succeeds_while_provider_transitioning() {
        #[derive(Debug)]
        struct TransitionProbe(u32);
        impl Service for TransitionProbe {}

        let ctx = Context::new_root();
        let fiber = Arc::new(Fiber::new());
        fiber.set_reload_context(&ctx);
        fiber.set_id(96_001);

        // Provide ON the registration fiber so the owner link exists — the
        // provide_on_fiber path used by RegistryService.
        let svc = Arc::new(TransitionProbe(7));
        ctx.provide_on_fiber(svc, &fiber);

        // Strict get refuses non-Active owners...
        assert!(ctx.get::<TransitionProbe>().is_none());

        for state in [
            FiberState::Loading,
            FiberState::Reloading,
            FiberState::Unloading { error: None },
            FiberState::Pending,
        ] {
            fiber.set_state(state.clone());
            let relaxed = ctx.get_relaxed::<TransitionProbe>();
            assert!(
                relaxed.is_some(),
                "relaxed read must succeed in {state:?}"
            );
            assert_eq!(
                relaxed.as_ref().map(|s| s.0),
                Some(7),
                "the transitioning value itself is served"
            );
        }

        // Terminal Failed stays refused even in relaxed mode.
        fiber.set_state(FiberState::Failed {
            error: Some("boom".into()),
        });
        assert!(
            ctx.get_relaxed::<TransitionProbe>().is_none(),
            "Failed owner must stay invisible to relaxed reads"
        );

        // Disposed owners stay refused too (dispose rests Inactive + flag).
        fiber.set_state(FiberState::Inactive { error: None });
        let _ = fiber.dispose().await;
        assert!(
            ctx.get_relaxed::<TransitionProbe>().is_none(),
            "disposed owner must stay invisible to relaxed reads"
        );
    }
}
