use parking_lot::{Mutex, RwLock};
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::context::Context;
use crate::service::{CordisError, Service};

pub(crate) type ReloadResult = Result<bool, CordisError>;
pub(crate) type ReloadRunner = Box<dyn FnMut(&Arc<Context>) -> ReloadResult + Send>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FiberState {
    Inactive {
        error: Option<String>,
    },
    Active {
        epoch: String,
    },
    /// A plugin activation is in flight (Cordis `LOADING`).
    Loading,
    /// Activation finished with an error; the plugin is not serving (Cordis `FAILED`).
    Failed {
        error: Option<String>,
    },
    Reloading,
    Unloading {
        error: Option<String>,
    },
}

pub struct Fiber {
    state: RwLock<FiberState>,
    inertia: Arc<tokio::sync::Mutex<()>>,
    acc: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    epoch: RwLock<String>,
    injects: RwLock<HashMap<TypeId, String>>, // TypeId -> type_name
    reload_runner: Mutex<Option<ReloadRunner>>,
    reload_ctx: Mutex<Option<std::sync::Weak<Context>>>,
    /// Peer-dependency version requirements per declared inject (`TypeId` ->
    /// `Option<u64>` encoded as `u64`; a missing key means unconstrained).
    /// Evaluated by [`Fiber::is_satisfied`] against
    /// [`crate::Context::provider_version`].
    inject_constraints: RwLock<HashMap<TypeId, u64>>,
    // Set when a late declare_inject raced an in-flight refresh (the inertia
    // guard was held), so the refresh loop folds the declaration in instead
    // of losing it between passes.
    pending_declare: AtomicBool,
    id: Mutex<Option<crate::FiberId>>,
    disposed: AtomicBool,
}

impl Fiber {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(FiberState::Inactive { error: None }),
            inertia: Arc::new(tokio::sync::Mutex::new(())),
            acc: Mutex::new(Vec::new()),
            epoch: RwLock::new(String::new()),
            injects: RwLock::new(HashMap::new()),
            reload_runner: Mutex::new(None),
            reload_ctx: Mutex::new(None),
            inject_constraints: RwLock::new(HashMap::new()),
            pending_declare: AtomicBool::new(false),
            id: Mutex::new(None),
            disposed: AtomicBool::new(false),
        }
    }

    pub fn declare_inject<T: Service>(&self) {
        let tid = TypeId::of::<T>();
        let name = std::any::type_name::<T>().to_string();
        self.injects.write().insert(tid, name);
        // Capture the registration context up front: the parking_lot mutex
        // below is not reentrant, so no guard may be held across
        // `reconcile_after_declare`, which re-locks the same mutex.
        let weak_ctx = self.reload_ctx.lock().clone();
        let fid = *self.id.lock();
        if let (Some(ctx), Some(fid)) = (weak_ctx.and_then(|w| w.upgrade()), fid) {
            if let Some(reflect) = ctx.get::<crate::ReflectService>() {
                reflect.register_dependent(tid, fid);
                let _ = reflect.ensure_notifier(tid);
            }
            // Metatheory delta #1 (resolved): a declaration landing on a
            // fiber that rests Active reconciles immediately instead of
            // waiting for the next external refresh.
            self.reconcile_after_declare(&ctx);
        }
    }

    /// Eagerly reconcile a freshly declared inject with the state machine.
    ///
    /// A declaration on a fiber resting `Active` must re-enter the machine
    /// right away: a satisfied declaration updates the epoch in place, an
    /// unsatisfied one drives the same transition shape [`Self::refresh`]
    /// uses (brief `Reloading`, effects undone, rest with the
    /// missing-dependency note). Declarations on `Inactive`/`Failed` fibers
    /// keep their historical behavior — they are evaluated at the fiber's
    /// next transition.
    fn reconcile_after_declare(&self, ctx: &Arc<Context>) {
        if !matches!(self.state(), FiberState::Active { .. }) {
            return;
        }
        let Ok(_guard) = self.inertia.try_lock() else {
            // An async refresh holds the inertia guard right now; it observes
            // this declaration through `pending_declare` and folds it into
            // its recompute loop.
            self.pending_declare.store(true, Ordering::Release);
            return;
        };
        let weak_ctx = self.reload_ctx.lock().clone();
        let reload_ctx = weak_ctx
            .and_then(|weak| weak.upgrade())
            .unwrap_or_else(|| ctx.clone());
        if self.is_satisfied(&reload_ctx) {
            let epoch = self.compute_epoch(&reload_ctx);
            self.set_epoch(epoch.clone());
            self.set_state(FiberState::Active { epoch });
            return;
        }
        self.set_state(FiberState::Reloading);
        let has_runner = self.reload_runner.lock().is_some();
        if has_runner {
            self.undo_effects();
        }
        self.set_state(FiberState::Inactive {
            error: Some("missing or inactive dependency".into()),
        });
        tracing::debug!(
            state = ?self.state(),
            "Cordis fiber deactivated by unsatisfied late inject declaration"
        );
    }

    /// Declare an inject on `T` with an optional peer-dependency version
    /// requirement. `None` is the historical, unconstrained behavior
    /// ([`Self::declare_inject`]); `Some(requirement)` additionally demands a
    /// same-major provider at or above the floor — see the scheme documented
    /// on [`crate::Context::VERSION_MAJOR_SCALE`].
    ///
    /// Lock discipline mirrors [`Self::declare_inject`]: short-lived lock
    /// acquisitions only, no guard held across reconciliation.
    pub fn declare_inject_versioned<T: Service>(&self, min_compatible: Option<u64>) {
        self.declare_inject::<T>();
        let tid = TypeId::of::<T>();
        match min_compatible {
            Some(requirement) => {
                self.inject_constraints.write().insert(tid, requirement);
            }
            None => {
                self.inject_constraints.write().remove(&tid);
            }
        }
        // The constraint may change satisfaction without changing the epoch;
        // re-enter the state machine exactly as the eager reconciliation for
        // structural declarations does. The reload_ctx guard must be released
        // before reconciling: `reconcile_after_declare` re-locks the same
        // non-reentrant mutex.
        let weak_ctx = self.reload_ctx.lock().clone();
        if let Some(ctx) = weak_ctx.and_then(|weak| weak.upgrade()) {
            self.reconcile_after_declare(&ctx);
        }
    }

    /// The recorded peer-version requirement for `tid`, if any.
    pub(crate) fn inject_constraint(&self, tid: TypeId) -> Option<u64> {
        self.inject_constraints.read().get(&tid).copied()
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

    pub(crate) fn set_epoch(&self, epoch: String) {
        *self.epoch.write() = epoch;
    }

    pub(crate) fn set_reload_runner(&self, runner: ReloadRunner) {
        *self.reload_runner.lock() = Some(runner);
    }

    pub(crate) fn set_id(&self, id: crate::FiberId) {
        *self.id.lock() = Some(id);
    }

    pub(crate) fn set_reload_context(&self, ctx: &Arc<Context>) {
        *self.reload_ctx.lock() = Some(Arc::downgrade(ctx));
    }

    pub(crate) fn run_runner(&self, ctx: &Arc<Context>) -> ReloadResult {
        // Do not hold the runner mutex while invoking plugin code. A provider
        // may call back into Context::provide/get, which can notify and queue a
        // refresh of this same fiber. Keep the mutable callback exclusive, but
        // release the mutex for the callback itself so those paths cannot
        // recursively acquire it.
        let mut runner = self.reload_runner.lock().take();
        let result = runner
            .as_mut()
            .map(|runner| runner(ctx))
            .unwrap_or(Ok(true));
        *self.reload_runner.lock() = runner;
        result
    }

    pub(crate) fn injected_type_ids(&self) -> Vec<TypeId> {
        self.injects.read().keys().copied().collect()
    }

    /// Compute epoch as ":type_name:version:..." sorted (monoid over
    /// concatenation). Injects carrying a peer-version requirement extend
    /// their fragment to "type_name:v<major>@<floor>:<structural>" so provider
    /// upgrades within the same major flip the epoch; unconstrained injects
    /// keep the historical fragment shape.
    pub fn compute_epoch(&self, ctx: &Arc<Context>) -> String {
        let injects = self.injects.read();
        if injects.is_empty() {
            return ":".to_string();
        }
        let mut frags: Vec<String> = Vec::new();
        for (tid, type_name) in injects.iter() {
            let version = ctx.get_version(*tid);
            match self.inject_constraint(*tid) {
                None => frags.push(format!("{}:{}", type_name, version)),
                // Constrained injects fold the semantic peer version in so a
                // same-major provider swap still flips the reactive epoch;
                // unconstrained fragments stay byte-identical to the
                // historical scheme.
                Some(requirement) => {
                    let p = ctx.provider_version(*tid);
                    frags.push(format!(
                        "{}:v{}@{}:{}",
                        type_name,
                        p / crate::Context::VERSION_MAJOR_SCALE,
                        requirement % crate::Context::VERSION_MAJOR_SCALE,
                        version
                    ));
                }
            }
        }
        frags.sort();
        format!(":{}", frags.join(":"))
    }

    /// Refresh recomputes dependency epoch and reruns registered plugins.
    ///
    /// Declarations that raced an in-flight refresh are folded in through the
    /// `pending_declare` flag: the loop re-runs until a pass observes no
    /// pending declaration, so a late `declare_inject` can never be lost
    /// between two refreshes.
    pub async fn refresh(&self, ctx: &Arc<Context>) {
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        loop {
            let _guard = self.inertia.lock().await;
            if self.disposed.load(Ordering::Acquire) {
                return;
            }
            let reload_ctx = self
                .reload_ctx
                .lock()
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .unwrap_or_else(|| ctx.clone());
            let new_epoch = self.compute_epoch(&reload_ctx);
            let old_epoch = self.epoch.read().clone();
            let previous = self.state();
            let satisfied = self.is_satisfied(&reload_ctx);
            if new_epoch == old_epoch
                && satisfied
                && matches!(previous, FiberState::Active { .. })
                && !self.pending_declare.swap(false, Ordering::AcqRel)
            {
                return;
            }

            self.set_state(FiberState::Reloading);
            tokio::task::yield_now().await;
            let has_runner = self.reload_runner.lock().is_some();
            if has_runner {
                self.undo_effects();
            }
            if !satisfied {
                self.set_state(FiberState::Inactive {
                    error: Some("missing or inactive dependency".into()),
                });
                if self.pending_declare.swap(false, Ordering::AcqRel) {
                    continue;
                }
                return;
            }

            let result = self.run_runner(&reload_ctx);
            match result {
                Ok(true) => {
                    self.set_epoch(new_epoch.clone());
                    self.set_state(FiberState::Active { epoch: new_epoch });
                }
                Ok(false) => {
                    self.set_state(FiberState::Inactive { error: None });
                }
                Err(error) => {
                    self.set_state(FiberState::Failed {
                        error: Some(error.to_string()),
                    });
                }
            }
            if previous != self.state() {
                tracing::debug!(from=?previous, to=?self.state(), "Cordis fiber transition");
            }
            if !self.pending_declare.swap(false, Ordering::AcqRel) {
                return;
            }
        }
    }

    /// A declared inject is satisfied iff its provider is available and — for
    /// injects declared via [`Self::declare_inject_versioned`] with a
    /// requirement — the provider's semantic peer version matches it:
    /// same major bucket (`p / 100_000 == r / 100_000`) and at least the
    /// requested floor (`p >= r`). Mismatch keeps the dependent `Inactive`;
    /// it never silently binds an incompatible provider. Legacy providers
    /// (version 0) satisfy only unconstrained injects.
    fn is_satisfied(&self, ctx: &Arc<Context>) -> bool {
        self.injects.read().keys().all(|tid| {
            if !ctx.is_available(*tid) {
                return false;
            }
            match self.inject_constraint(*tid) {
                None => true,
                Some(requirement) => {
                    let p = ctx.provider_version(*tid);
                    p / crate::Context::VERSION_MAJOR_SCALE
                        == requirement / crate::Context::VERSION_MAJOR_SCALE
                        && p >= requirement
                }
            }
        })
    }

    fn undo_effects(&self) {
        // Pop before invoking user/plugin cleanup. Cleanup may provide another
        // service and push a new undo onto this fiber; holding acc while it
        // runs would recursively lock the same non-reentrant mutex.
        while let Some(undo) = { self.acc.lock().pop() } {
            undo();
        }
    }

    pub async fn dispose(&self) {
        let _guard = self.inertia.lock().await;
        self.disposed.store(true, Ordering::Release);
        self.set_state(FiberState::Unloading { error: None });
        while let Some(undo) = { self.acc.lock().pop() } {
            undo();
        }
        self.set_state(FiberState::Inactive { error: None });
        self.set_epoch(String::new());
    }

    /// Apply a config change through the same dependency reload runner used by
    /// reactive refresh. Existing registration effects are undone before the
    /// plugin is applied again.
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

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex as ParkingMutex;

    #[derive(Debug)]
    struct LateProbe(pub i32);
    impl Service for LateProbe {}

    /// Former delta #1, satisfied leg: a declaration landing on an
    /// already-Active fiber folds into the epoch immediately.
    #[tokio::test]
    async fn late_satisfied_declaration_updates_active_fiber_without_refresh() {
        let ctx = Context::new_root();
        let fiber = Arc::new(Fiber::new());
        fiber.set_reload_context(&ctx);
        fiber.set_id(70_001);
        fiber.set_state(FiberState::Active { epoch: ":".into() });
        ctx.provide(LateProbe(3));
        fiber.declare_inject::<LateProbe>();
        assert!(
            matches!(fiber.state(), FiberState::Active { .. }),
            "satisfied declaration must keep the fiber Active: {:?}",
            fiber.state()
        );
        assert!(
            fiber.epoch().contains("LateProbe"),
            "epoch must fold the declared inject: {}",
            fiber.epoch()
        );
    }

    /// Former delta #1, unsatisfied leg: a declaration of a missing
    /// dependency drives the Active fiber to rest Inactive right away,
    /// mirroring refresh's transition shape.
    #[tokio::test]
    async fn late_unsatisfied_declaration_deactivates_immediately() {
        #[derive(Debug)]
        struct NeverProvided;
        impl Service for NeverProvided {}

        let ctx = Context::new_root();
        let fiber = Arc::new(Fiber::new());
        fiber.set_reload_context(&ctx);
        fiber.set_id(70_002);
        fiber.set_state(FiberState::Active { epoch: ":".into() });
        fiber.declare_inject::<NeverProvided>();
        match fiber.state() {
            FiberState::Inactive { error: Some(note) } => {
                assert!(
                    note.contains("missing or inactive dependency"),
                    "unexpected note: {note}"
                );
            }
            other => panic!("expected eager deactivation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fiber_refresh_passes_through_reloading() {
        let ctx = Context::new_root();
        let fiber = Arc::new(Fiber::new());
        let seen = Arc::new(ParkingMutex::new(Vec::new()));
        let f = fiber.clone();
        let s = seen.clone();
        let poller = tokio::spawn(async move {
            loop {
                s.lock().push(f.state());
                tokio::task::yield_now().await;
            }
        });
        fiber.refresh(&ctx).await;
        poller.abort();
        let _ = poller.await;
        let states = seen.lock().clone();
        assert!(
            states.iter().any(|st| matches!(st, FiberState::Reloading)),
            "poller must have seen Reloading, got {:?}",
            states
        );
        assert!(matches!(
            fiber.state(),
            FiberState::Active { .. } | FiberState::Inactive { .. }
        ));
    }

    #[tokio::test]
    async fn fiber_dispose_passes_through_unloading() {
        let fiber = Arc::new(Fiber::new());
        let snap = Arc::new(ParkingMutex::new(None));
        let snap2 = snap.clone();
        let f = fiber.clone();
        fiber.push_undo(Box::new(move || {
            *snap2.lock() = Some(f.state());
        }));
        fiber.dispose().await;
        let during = snap.lock().clone();
        assert_eq!(during, Some(FiberState::Unloading { error: None }));
        assert_eq!(fiber.state(), FiberState::Inactive { error: None });
    }
}
