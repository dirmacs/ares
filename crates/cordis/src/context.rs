use parking_lot::RwLock;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use crate::effect::{Disposable, Effect};
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

    pub fn isolate_type(self: &Arc<Self>, tid: TypeId, label: impl Into<Symbol>) -> Arc<Self> {
        let mut parent_isolate = self.isolate.read().clone();
        parent_isolate.insert(tid, label.into());
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
