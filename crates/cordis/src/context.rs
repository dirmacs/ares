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

/// Erased getter for one name-keyed accessor. Receives the context so a
/// computed property can compose over services; MUST NOT consult the
/// `internal/get` waterfall (accessor reads bypass interception by design).
pub type AccessorGetter =
    Arc<dyn Fn(&Context) -> Result<Option<Arc<dyn Any + Send + Sync>>, CordisError> + Send + Sync>;

/// Erased setter for one name-keyed accessor. Bypasses the `internal/set`
/// waterfall by construction.
pub type AccessorSetter =
    Arc<dyn Fn(&Context, Arc<dyn Any + Send + Sync>) -> Result<(), CordisError> + Send + Sync>;

/// Declarative descriptor handed to [`Context::register_accessor`].
#[derive(Clone, Default)]
pub struct Accessor {
    getter: Option<AccessorGetter>,
    setter: Option<AccessorSetter>,
}

impl Accessor {
    /// Readable property: reads resolve through `getter`, writes are refused
    /// with [`CordisError::ReadOnlyProperty`].
    pub fn read_only<F>(getter: F) -> Self
    where
        F: Fn(&Context) -> Result<Option<Arc<dyn Any + Send + Sync>>, CordisError>
            + Send
            + Sync
            + 'static,
    {
        Self {
            getter: Some(Arc::new(getter)),
            setter: None,
        }
    }

    /// Read-write property.
    pub fn read_write<G, S>(getter: G, setter: S) -> Self
    where
        G: Fn(&Context) -> Result<Option<Arc<dyn Any + Send + Sync>>, CordisError>
            + Send
            + Sync
            + 'static,
        S: Fn(&Context, Arc<dyn Any + Send + Sync>) -> Result<(), CordisError>
            + Send
            + Sync
            + 'static,
    {
        Self {
            getter: Some(Arc::new(getter)),
            setter: Some(Arc::new(setter)),
        }
    }

    /// Write-only property: reads resolve `None`, writes go through `setter`.
    pub fn setter_only<S>(setter: S) -> Self
    where
        S: Fn(&Context, Arc<dyn Any + Send + Sync>) -> Result<(), CordisError>
            + Send
            + Sync
            + 'static,
    {
        Self {
            getter: None,
            setter: Some(Arc::new(setter)),
        }
    }
}

/// Shared registration record: one declaration plus every name bound to it
/// (the declared name and any [`Context::alias`] alternates). Disposing the
/// handle removes ALL bound names in one shot.
struct AccessorSlot {
    getter: Option<AccessorGetter>,
    setter: Option<AccessorSetter>,
    names: parking_lot::Mutex<Vec<String>>,
}

/// Handle returned by [`Context::register_accessor`]. Call
/// [`EffectHandle::dispose`] to remove the accessor (and its aliases).
pub struct EffectHandle {
    ctx: Weak<Context>,
    slot: Weak<AccessorSlot>,
}

impl EffectHandle {
    /// Remove the registered accessor and every alias pointing at it.
    /// Returns `true` when the declaration was still live and got removed.
    pub fn dispose(self) -> bool {
        let (Some(ctx), Some(slot)) = (self.ctx.upgrade(), self.slot.upgrade()) else {
            return false;
        };
        let names = slot.names.lock().clone();
        let mut accessors = ctx.accessors.write();
        let mut removed = false;
        for name in names {
            if accessors
                .get(&name)
                .is_some_and(|bound| std::sync::Arc::ptr_eq(bound, &slot))
            {
                accessors.remove(&name);
                removed = true;
            }
        }
        removed
    }
}

pub struct Context {
    store: RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    isolate: RwLock<HashMap<TypeId, Symbol>>,
    /// Layered intercept overrides per TypeId, ordered OUTERMOST..INNERMOST.
    /// Every set APPENDS a layer; the effective value is the innermost (the
    /// last element). See [`Context::intercept_chain`].
    intercept: RwLock<HashMap<TypeId, Vec<Arc<dyn Any + Send + Sync>>>>,
    /// Name-keyed computed-property accessors living beside the TypeId
    /// service store. Accessor reads/writes deliberately bypass the
    /// `internal/get` / `internal/set` intercept waterfalls.
    accessors: RwLock<HashMap<String, std::sync::Arc<AccessorSlot>>>,
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
            accessors: RwLock::new(HashMap::new()),
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
            accessors: RwLock::new(HashMap::new()),
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
            accessors: RwLock::new(HashMap::new()),
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
        // Append a fresh layer on the forked frame; effective stays innermost.
        child.intercept.write().entry(tid).or_default().push(any);
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
        // C1 `internal/set` write veto: a failing chain refuses THIS write
        // by returning the PREVIOUS value for `tid` (the old binding stays
        // fully intact — no store/owners/version mutation happens). With no
        // listener registered the consult is two map reads.
        if let Some(events) = self.get_unintercepted::<crate::EventsService>() {
            if crate::events::blocking_intercept_set(&events, std::any::type_name::<T>()).is_err() {
                tracing::info!(
                    service = std::any::type_name::<T>(),
                    "internal/set vetoed provider write; previous value stays"
                );
                let prev_any = self.store.read().get(&tid).cloned();
                return prev_any
                    .and_then(|any| any.downcast::<T>().ok())
                    .unwrap_or(svc);
            }
        }
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
        // Layered: appending keeps the previous outer layers intact and makes
        // `any` the new effective (innermost) value.
        self.intercept.write().entry(tid).or_default().push(any);
    }

    /// Read the EFFECTIVE (innermost) intercept override without removing it.
    pub(crate) fn peek_intercept_untyped(&self, tid: TypeId) -> Option<Arc<dyn Any + Send + Sync>> {
        self.intercept
            .read()
            .get(&tid)
            .and_then(|layers| layers.last())
            .cloned()
    }

    /// Pop the innermost intercept layer. The key is removed entirely once
    /// the last layer goes.
    pub(crate) fn remove_intercept_untyped(&self, tid: TypeId) {
        let mut intercept = self.intercept.write();
        if let Some(layers) = intercept.get_mut(&tid) {
            layers.pop();
            if layers.is_empty() {
                intercept.remove(&tid);
            }
        }
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
            if let Some(any) = self.intercept.read().get(&tid).and_then(|l| l.last()) {
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
        self.parent
            .as_ref()
            .and_then(|parent| parent.get_relaxed::<T>())
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
        // C1 `internal/get` strict-read interception: consult the veto chain
        // ONCE per top-level read. Redirect skips this frame's bindings so a
        // parent binding serves the read; Refuse fails the lookup outright.
        if let Some(events) = self.get_unintercepted::<crate::EventsService>() {
            match crate::events::blocking_intercept_get(&events, std::any::type_name::<T>()) {
                crate::events::ReadVerdict::Pass => {}
                crate::events::ReadVerdict::RedirectFrame => {
                    return self.get_from_parent_frame::<T>();
                }
                crate::events::ReadVerdict::Refuse => return None,
            }
        }
        self.get_impl::<T>()
    }

    /// Internal read used by the interception bridges themselves: resolves a
    /// service WITHOUT re-consulting `internal/get` (the thread-local fence
    /// already guarantees no recursion; this path additionally avoids the
    /// bridge call for kernel-internal lookups).
    fn get_unintercepted<T: Service>(&self) -> Option<Arc<T>> {
        self.get_impl::<T>()
    }

    /// Continue a redirected strict read at the PARENT frame, skipping this
    /// context's store + intercept bindings entirely. The root frame answers
    /// `None` — there is nothing above to serve from.
    fn get_from_parent_frame<T: Service>(&self) -> Option<Arc<T>> {
        let Some(parent) = &self.parent else {
            return None;
        };
        if parent.isolate_label(TypeId::of::<T>()) != self.isolate_label(TypeId::of::<T>()) {
            return None;
        }
        // The parent lookup runs under the same fence as the original read,
        // so it will not re-consult the veto chain.
        parent.get_impl::<T>()
    }

    /// The historical strict-read body shared by [`Self::get`] and the two
    /// helpers above.
    fn get_impl<T: Service>(&self) -> Option<Arc<T>> {
        let tid = TypeId::of::<T>();
        // Isolate-labeled TypeIds resolve from store / isolate parent walk.
        // Unlabeled TypeIds still let the EFFECTIVE (innermost) intercept win.
        if self.isolate_label(tid).is_none() {
            if let Some(any) = self.intercept.read().get(&tid).and_then(|l| l.last()) {
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
                    parent.get_impl::<T>()
                } else {
                    None
                }
            });
        }
        self.parent
            .as_ref()
            .and_then(|parent| parent.get_impl::<T>())
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
        if self.isolate_label(tid).is_none()
            && self
                .intercept
                .read()
                .get(&tid)
                .is_some_and(|layers| !layers.is_empty())
        {
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
    /// APPENDS a layer: the effective value becomes the innermost (this one)
    /// while outer layers stay inspectable through [`Self::intercept_chain`].
    pub fn bind_intercept<T: Service>(&self, val: T) {
        let tid = TypeId::of::<T>();
        let any: Arc<dyn Any + Send + Sync> = Arc::new(val);
        self.intercept.write().entry(tid).or_default().push(any);
    }
    // --- Name-keyed computed-property accessors ---------------------------

    /// Register a computed property under `name` beside the TypeId service
    /// store. Duplicate declarations (including alias collisions) are
    /// rejected with [`CordisError::DuplicateProvider`]. The returned
    /// [`EffectHandle`] removes the declaration (and any aliases) on
    /// dispose.
    ///
    /// Accessor reads/writes BYPASS the `internal/get` / `internal/set`
    /// intercept waterfalls entirely — resolving an accessor never consults
    /// or re-enters a veto chain.
    pub fn register_accessor(
        self: &Arc<Self>,
        name: &str,
        accessor: Accessor,
    ) -> Result<EffectHandle, CordisError> {
        let mut accessors = self.accessors.write();
        if accessors.contains_key(name) {
            return Err(CordisError::DuplicateProvider {
                name: name.to_string(),
                owner: "accessor".to_string(),
            });
        }
        let slot = std::sync::Arc::new(AccessorSlot {
            getter: accessor.getter,
            setter: accessor.setter,
            names: parking_lot::Mutex::new(vec![name.to_string()]),
        });
        accessors.insert(name.to_string(), slot.clone());
        Ok(EffectHandle {
            ctx: Arc::downgrade(self),
            slot: Arc::downgrade(&slot),
        })
    }

    /// Bind `alias` as an alternate name resolving through the SAME
    /// registration as `target` — same getter/setter, disposed together.
    pub fn alias(self: &Arc<Self>, alias: &str, target: &str) -> Result<(), CordisError> {
        let mut accessors = self.accessors.write();
        let slot = accessors.get(target).cloned().ok_or_else(|| {
            CordisError::ServiceNotFound(format!(
                "cannot alias '{alias}': no property named '{target}'"
            ))
        })?;
        if accessors.contains_key(alias) {
            return Err(CordisError::DuplicateProvider {
                name: alias.to_string(),
                owner: "accessor".to_string(),
            });
        }
        slot.names.lock().push(alias.to_string());
        accessors.insert(alias.to_string(), slot);
        Ok(())
    }

    /// Resolve `name` through its accessor (bypassing all interception
    /// waterfalls). Undeclared names and write-only properties resolve
    /// `None`; use [`Self::read_property_typed`] for downcast checking.
    pub fn read_property(
        &self,
        name: &str,
    ) -> Result<Option<Arc<dyn Any + Send + Sync>>, CordisError> {
        let slot = self.accessors.read().get(name).cloned();
        let Some(slot) = slot else {
            return Ok(None);
        };
        match &slot.getter {
            Some(getter) => getter(self),
            None => Ok(None),
        }
    }

    /// Typed accessor read: a value that fails to downcast to `T` is
    /// [`CordisError::PropertyTypeMismatch`], never a silent `None`.
    pub fn read_property_typed<T: Any + Send + Sync>(
        &self,
        name: &str,
    ) -> Result<Option<Arc<T>>, CordisError> {
        match self.read_property(name)? {
            None => Ok(None),
            Some(any) => {
                any.downcast::<T>()
                    .map(Some)
                    .map_err(|_| CordisError::PropertyTypeMismatch {
                        name: name.to_string(),
                        expected: std::any::type_name::<T>().to_string(),
                    })
            }
        }
    }

    /// Write `value` to `name` through its accessor. A fully undeclared
    /// name is refused MissingService-style ("cannot set property"); a
    /// declared-but-setter-less name is refused
    /// [`CordisError::ReadOnlyProperty`]. Never consults the
    /// `internal/set` waterfall.
    pub fn write_property(
        self: &Arc<Self>,
        name: &str,
        value: Arc<dyn Any + Send + Sync>,
    ) -> Result<(), CordisError> {
        let slot = self.accessors.read().get(name).cloned();
        let Some(slot) = slot else {
            return Err(CordisError::ServiceNotFound(format!(
                "cannot set property '{name}': no accessor declared"
            )));
        };
        let Some(setter) = &slot.setter else {
            return Err(CordisError::ReadOnlyProperty(name.to_string()));
        };
        setter(self, value)
    }

    // --- Layered intercept chains -----------------------------------------

    /// All intercept layers for `tid` visible from this frame, ordered
    /// OUTERMOST..INNERMOST (ancestor frames first, this frame's appended
    /// layers last). The innermost element is the effective value every
    /// existing single-value getter returns.
    pub fn intercept_chain(&self, tid: TypeId) -> Vec<Arc<dyn Any + Send + Sync>> {
        let mut chain = match &self.parent {
            // An isolate label is a realm boundary: layers beyond it do not
            // leak in, mirroring the strict-read parent walk.
            Some(parent)
                if !self.isolate.read().contains_key(&tid)
                    || parent.isolate_label(tid) == self.isolate_label(tid) =>
            {
                parent.intercept_chain(tid)
            }
            _ => Vec::new(),
        };
        if let Some(layers) = self.intercept.read().get(&tid) {
            chain.extend(layers.iter().cloned());
        }
        chain
    }

    /// Structural equality for two intercepted chains, used by
    /// restart-decision comparisons: same length and every layer pair the
    /// SAME shared instance (`Arc::ptr_eq`). Erased values carry no
    /// comparable contract, so identity is the only honest structural test;
    /// freshly-built values therefore compare unequal by design.
    pub fn chains_structurally_equal(
        a: &[Arc<dyn Any + Send + Sync>],
        b: &[Arc<dyn Any + Send + Sync>],
    ) -> bool {
        a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| Arc::ptr_eq(x, y))
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
            assert!(relaxed.is_some(), "relaxed read must succeed in {state:?}");
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

#[cfg(test)]
mod accessor_tests {
    use super::*;
    use parking_lot::Mutex as StdMutex;

    #[derive(Debug, PartialEq)]
    struct PropValue(pub u64);

    fn read_cell_getter(
        cell: Arc<StdMutex<u64>>,
    ) -> impl Fn(&Context) -> Result<Option<Arc<dyn Any + Send + Sync>>, CordisError>
           + Send
           + Sync
           + 'static {
        move |_| {
            Ok(Some(
                Arc::new(PropValue(*cell.lock())) as Arc<dyn Any + Send + Sync>
            ))
        }
    }

    #[test]
    fn accessor_read_write_roundtrip() {
        let ctx = Context::new_root();
        let cell = Arc::new(StdMutex::new(1u64));
        let write_cell = cell.clone();
        let _handle = ctx
            .register_accessor(
                "quota",
                Accessor::read_write(
                    read_cell_getter(cell.clone()),
                    move |_ctx, value: Arc<dyn Any + Send + Sync>| {
                        let v = value
                            .downcast::<PropValue>()
                            .map_err(|_| CordisError::Internal("bad property type".into()))?;
                        *write_cell.lock() = v.0;
                        Ok(())
                    },
                ),
            )
            .unwrap();

        let got = ctx
            .read_property_typed::<PropValue>("quota")
            .unwrap()
            .unwrap();
        assert_eq!(*got, PropValue(1));
        ctx.write_property("quota", Arc::new(PropValue(42)))
            .unwrap();
        let got = ctx
            .read_property_typed::<PropValue>("quota")
            .unwrap()
            .unwrap();
        assert_eq!(*got, PropValue(42));
        assert_eq!(*cell.lock(), 42);

        // A wrong typed read is a PropertyTypeMismatch, never a silent None.
        match ctx.read_property_typed::<String>("quota") {
            Err(CordisError::PropertyTypeMismatch { name, .. }) => assert_eq!(name, "quota"),
            other => panic!("expected PropertyTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_accessor_declaration_rejected() {
        let ctx = Context::new_root();
        ctx.register_accessor("dup", Accessor::read_only(|_| Ok(None)))
            .expect("first declaration wins");
        match ctx.register_accessor("dup", Accessor::read_only(|_| Ok(None))) {
            Err(CordisError::DuplicateProvider { name, owner }) => {
                assert_eq!(name, "dup");
                assert_eq!(owner, "accessor");
            }
            Err(other) => panic!("expected DuplicateProvider, got {other:?}"),
            Ok(_) => panic!("duplicate declaration must be rejected"),
        }
    }

    #[test]
    fn readonly_property_rejects_set() {
        let ctx = Context::new_root();
        let _handle = ctx
            .register_accessor(
                "ro",
                Accessor::read_only(read_cell_getter(Arc::new(StdMutex::new(7u64)))),
            )
            .unwrap();
        let err = ctx
            .write_property("ro", Arc::new(PropValue(9)))
            .unwrap_err();
        assert!(matches!(err, CordisError::ReadOnlyProperty(ref n) if n == "ro"));
        // The stored resolution is untouched.
        assert_eq!(
            *ctx.read_property_typed::<PropValue>("ro").unwrap().unwrap(),
            PropValue(7)
        );
    }

    #[test]
    fn dispose_accessor_resolves_none() {
        let ctx = Context::new_root();
        let handle = ctx
            .register_accessor("gone", Accessor::read_only(|_| Ok(None)))
            .unwrap();
        assert!(ctx.read_property("gone").unwrap().is_none());
        assert!(handle.dispose(), "live handle reports removal");
        // Still resolves none afterwards, but writes now hit the undeclared path.
        assert!(ctx.read_property("gone").unwrap().is_none());
        let err = ctx
            .write_property("gone", Arc::new(PropValue(1)))
            .unwrap_err();
        assert!(
            matches!(err, CordisError::ServiceNotFound(ref m) if m.contains("cannot set property")),
            "unexpected error: {err}"
        );
        // A re-registered declaration is disposable again.
        let handle2 = ctx
            .register_accessor("gone2", Accessor::read_only(|_| Ok(None)))
            .unwrap();
        assert!(handle2.dispose());
        assert!(ctx.read_property("gone2").unwrap().is_none());
    }

    #[test]
    fn alias_resolves_same_value() {
        let ctx = Context::new_root();
        let cell = Arc::new(StdMutex::new(5u64));
        let write_cell = cell.clone();
        let handle = ctx
            .register_accessor(
                "primary",
                Accessor::read_write(
                    read_cell_getter(cell),
                    move |_ctx, value: Arc<dyn Any + Send + Sync>| {
                        *write_cell.lock() = value.downcast::<PropValue>().unwrap().0;
                        Ok(())
                    },
                ),
            )
            .unwrap();
        ctx.alias("nick", "primary").expect("alias binds");

        // Same getter through the alias.
        assert_eq!(
            *ctx.read_property_typed::<PropValue>("nick")
                .unwrap()
                .unwrap(),
            PropValue(5)
        );
        // Same setter through the alias.
        ctx.write_property("nick", Arc::new(PropValue(6))).unwrap();
        assert_eq!(
            *ctx.read_property_typed::<PropValue>("primary")
                .unwrap()
                .unwrap(),
            PropValue(6)
        );

        // Collisions and unknown targets are refused.
        assert!(matches!(
            ctx.alias("nick", "primary"),
            Err(CordisError::DuplicateProvider { .. })
        ));
        assert!(matches!(
            ctx.alias("x", "missing"),
            Err(CordisError::ServiceNotFound(_))
        ));

        // Disposing the original registration removes BOTH names.
        assert!(handle.dispose());
        assert!(ctx.read_property("primary").unwrap().is_none());
        assert!(ctx.read_property("nick").unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accessor_bypasses_intercept_waterfalls() {
        struct Marker;
        impl crate::Service for Marker {}

        let ctx = Context::new_root();
        let events = Arc::new(crate::EventsService::new());
        ctx.provide_arc(events.clone());

        // Refuse every strict service read and veto every service write.
        let _get_gate = events.on(crate::events::INTERNAL_GET_EVENT.into(), |_p| async move {
            Ok(serde_json::json!({ "refuse": true }))
        });
        let _set_gate = events.on(crate::events::INTERNAL_SET_EVENT.into(), |_p| async move {
            Ok(serde_json::json!("vetoed"))
        });

        // Sanity: with the gates up, the strict paths ARE intercepted.
        ctx.provide(Marker);
        assert!(
            ctx.get::<Marker>().is_none(),
            "internal/get waterfall must refuse strict reads in this test"
        );

        // The accessor path ignores both waterfalls entirely.
        let cell = Arc::new(StdMutex::new(3u64));
        let write_cell = cell.clone();
        let _handle = ctx
            .register_accessor(
                "open",
                Accessor::read_write(
                    read_cell_getter(cell),
                    move |_c, value: Arc<dyn Any + Send + Sync>| {
                        *write_cell.lock() = value.downcast::<PropValue>().unwrap().0;
                        Ok(())
                    },
                ),
            )
            .unwrap();
        assert_eq!(
            *ctx.read_property_typed::<PropValue>("open")
                .unwrap()
                .unwrap(),
            PropValue(3),
            "accessor read bypasses internal/get"
        );
        ctx.write_property("open", Arc::new(PropValue(4))).unwrap();
        assert_eq!(
            *ctx.read_property_typed::<PropValue>("open")
                .unwrap()
                .unwrap(),
            PropValue(4),
            "accessor write bypasses internal/set"
        );
    }
}

#[cfg(test)]
mod intercept_chain_tests {
    use super::*;

    #[derive(Debug)]
    struct LayerSvc(pub u64);
    impl crate::Service for LayerSvc {}

    #[tokio::test]
    async fn chained_layers_append_innermost_effective() {
        let ctx = Context::new_root();
        ctx.bind_intercept(LayerSvc(1));
        assert_eq!(ctx.get::<LayerSvc>().unwrap().0, 1);
        ctx.bind_intercept(LayerSvc(2));
        assert_eq!(ctx.get::<LayerSvc>().unwrap().0, 2, "innermost layer wins");
        let chain = ctx.intercept_chain(TypeId::of::<LayerSvc>());
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].clone().downcast::<LayerSvc>().unwrap().0, 1);
        assert_eq!(chain[1].clone().downcast::<LayerSvc>().unwrap().0, 2);
    }

    #[tokio::test]
    async fn intercept_chain_returns_all_layers_in_order() {
        let root = Context::new_root();
        let mid = root.intercept(LayerSvc(10));
        let leaf = mid.intercept(LayerSvc(11));
        leaf.bind_intercept(LayerSvc(12));

        let chain = leaf.intercept_chain(TypeId::of::<LayerSvc>());
        assert_eq!(chain.len(), 3);
        let vals: Vec<u64> = chain
            .iter()
            .map(|a| a.clone().downcast::<LayerSvc>().unwrap().0)
            .collect();
        assert_eq!(vals, vec![10, 11, 12], "outermost..innermost order");
        assert_eq!(leaf.get::<LayerSvc>().unwrap().0, 12);

        // Structural equality: the same chain matches its own snapshot but
        // not an identically-built chain of fresh values.
        assert!(Context::chains_structurally_equal(
            &chain,
            &leaf.intercept_chain(TypeId::of::<LayerSvc>())
        ));
        let fresh_root = Context::new_root();
        let fresh_mid = fresh_root.intercept(LayerSvc(10));
        let fresh_leaf = fresh_mid.intercept(LayerSvc(11));
        fresh_leaf.bind_intercept(LayerSvc(12));
        assert!(!Context::chains_structurally_equal(
            &chain,
            &fresh_leaf.intercept_chain(TypeId::of::<LayerSvc>())
        ));
    }

    #[tokio::test]
    async fn inject_appends_layer() {
        let ctx = Context::new_root();
        let tid = TypeId::of::<LayerSvc>();

        // Untyped plugin-style injection APPENDS instead of replacing.
        ctx.bind_intercept_untyped(tid, Arc::new(LayerSvc(20)) as Arc<dyn Any + Send + Sync>);
        ctx.bind_intercept_untyped(tid, Arc::new(LayerSvc(21)) as Arc<dyn Any + Send + Sync>);
        assert_eq!(ctx.intercept_chain(tid).len(), 2);
        assert_eq!(ctx.get::<LayerSvc>().unwrap().0, 21);

        // Popping the innermost layer restores the outer one as effective.
        ctx.remove_intercept_untyped(tid);
        assert_eq!(ctx.intercept_chain(tid).len(), 1);
        assert_eq!(ctx.get::<LayerSvc>().unwrap().0, 20);
        ctx.remove_intercept_untyped(tid);
        assert!(ctx.intercept_chain(tid).is_empty());
        assert!(ctx.get::<LayerSvc>().is_none());
    }
}
