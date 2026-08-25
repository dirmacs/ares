use parking_lot::{Mutex, RwLock};
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::context::Context;
use crate::service::{CordisError, Service};

pub(crate) type ReloadResult = Result<bool, CordisError>;
pub(crate) type ReloadRunner = Box<dyn FnMut(&Arc<Context>) -> ReloadResult + Send>;

/// Maximum time a lifecycle transition waits for the fiber's inertia guard
/// before giving up with [`CordisError::Fiber`]. Bounds the blast radius of a
/// hung plugin apply: dispose and refresh fail fast (naming the stuck fiber)
/// instead of parking forever behind a transition that never yields.
pub const TRANSITION_WAIT: Duration = Duration::from_secs(10);

#[cfg(test)]
pub(crate) static TRANSITION_WAIT_OVERRIDE: std::sync::OnceLock<Duration> =
    std::sync::OnceLock::new();

fn transition_wait() -> Duration {
    #[cfg(test)]
    if let Some(wait) = TRANSITION_WAIT_OVERRIDE.get() {
        return *wait;
    }
    TRANSITION_WAIT
}

/// Same-thread reentrancy ledger for the inertia guard.
///
/// The sync-callback model (reload runners invoke plugin code synchronously,
/// which can call back into `provide`/`notify`) makes recursive deadlock a
/// same-thread phenomenon: `tokio::sync::Mutex` blocks the whole thread on a
/// second acquisition. This set records `(thread id, fiber id)` pairs while a
/// bounded acquisition holds the guard; acquiring again for a pair already
/// present is an immediate "reentrant" error rather than a bounded wait that
/// would burn its whole timeout on itself.
///
/// Honest scope: this detects SAME-THREAD reentrancy only. Cross-task
/// contention is handled by the bounded wait and reported as "stuck".
type HeldTransitions = Mutex<std::collections::HashSet<(std::thread::ThreadId, u64)>>;
static HELD_TRANSITIONS: std::sync::LazyLock<HeldTransitions> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// RAII marker for one held transition: inserts `(current thread, fid)` on
/// construction, removes it on drop (including early returns and panics).
struct TransitionGuard {
    fid: u64,
}

/// Held transition: owns both the inertia guard and the reentrancy-ledger
/// slot. Fields are declared ledger-first so drop unregisters `(thread, fid)`
/// BEFORE releasing the mutex — a successor acquiring the guard never
/// observes a stale ledger entry from the previous holder.
struct TransitionLease {
    _ledger: TransitionGuard,
    _lock: tokio::sync::OwnedMutexGuard<()>,
}

impl TransitionGuard {
    fn acquire(fid: u64) -> Result<Self, CordisError> {
        let mut held = HELD_TRANSITIONS.lock();
        let key = (std::thread::current().id(), fid);
        if !held.insert(key) {
            return Err(CordisError::Fiber(format!(
                "reentrant transition on fiber {fid}"
            )));
        }
        Ok(Self { fid })
    }
}

impl Drop for TransitionGuard {
    fn drop(&mut self) {
        HELD_TRANSITIONS
            .lock()
            .remove(&(std::thread::current().id(), self.fid));
    }
}

/// Debug label + registration timestamp for one entry on a fiber's undo
/// accumulator. Our undos are anonymous closures; this is the minimal
/// introspection surface (labels only) — deliberately NOT an effect tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoMeta {
    pub label: String,
    /// Milliseconds since the Unix epoch at push time.
    pub registered_at_ms: u64,
}

impl Default for UndoMeta {
    fn default() -> Self {
        Self {
            label: "unnamed".into(),
            registered_at_ms: unix_now_ms(),
        }
    }
}

impl UndoMeta {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            registered_at_ms: unix_now_ms(),
        }
    }
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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

/// One registered undo: its introspection metadata plus the teardown closure.
type UndoEntry = (UndoMeta, Box<dyn FnOnce() + Send>);

pub struct Fiber {
    state: RwLock<FiberState>,
    inertia: Arc<tokio::sync::Mutex<()>>,
    acc: Mutex<Vec<UndoEntry>>,
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
    ///
    /// The inertia guard is acquired through [`Self::acquire_transition`]:
    /// same-thread reentrancy errors immediately and cross-task contention is
    /// bounded by [`TRANSITION_WAIT`]. A timed-out refresh surfaces the error
    /// as a `Failed` state so the fiber stays inspectable.
    pub async fn refresh(&self, ctx: &Arc<Context>) {
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        let fid = *self.id.lock().get_or_insert(0);
        let Ok(_guard) = self.acquire_transition(fid).await else {
            return;
        };
        loop {
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

    /// Non-blocking probe of the inertia guard: `true` when no lifecycle
    /// transition (refresh/update/dispose) currently holds it. Never blocks;
    /// a momentary `false` only means a transition was mid-flight at probe
    /// time, not that one is stuck.
    pub fn is_idle(&self) -> bool {
        // tokio's TryLockError is a unit struct; Err(_) means contended.
        self.inertia.try_lock().is_ok()
    }

    /// Bounded wait for the fiber to go idle: resolves `true` as soon as the
    /// inertia guard can be acquired (and releases it immediately), or
    /// `false` once [`transition_wait()`] elapses behind a holder — with a
    /// warn log naming the fiber id, mirroring the stuck-transition report
    /// of [`Self::acquire_transition`]. Unlike the lifecycle transitions this
    /// is a pure OBSERVATION call: it never mutates state and never enters
    /// the reentrancy ledger, so it is safe to call from inside a running
    /// transition on this same fiber.
    pub async fn wait_idle(&self) -> bool {
        let fid = *self.id.lock().get_or_insert(0);
        if self.inertia.try_lock().is_ok() {
            return true;
        }
        match tokio::time::timeout(transition_wait(), Arc::clone(&self.inertia).lock_owned()).await
        {
            // Guard acquired then dropped immediately: idle confirmed.
            Ok(_guard) => true,
            Err(_elapsed) => {
                let ms = transition_wait().as_millis();
                tracing::warn!("fiber {fid} still busy in transition over {ms}ms");
                false
            }
        }
    }

    /// Extract the human-readable error carried by a resting terminal state:
    /// `Failed{error}`, `Inactive{error}`, or `Unloading{error}`. Active,
    /// Loading, and Reloading fibers report `None`.
    pub fn error(&self) -> Option<String> {
        match &*self.state.read() {
            FiberState::Failed { error }
            | FiberState::Inactive { error }
            | FiberState::Unloading { error } => error.clone(),
            _ => None,
        }
    }

    /// Bounded acquisition of the inertia guard for one lifecycle transition:
    /// same-thread reentrancy on an already-held fiber errors immediately,
    /// otherwise the wait for a contending holder is capped at
    /// [`transition_wait()`] before failing with [`CordisError::Fiber`]
    /// naming the fiber id. The returned guard releases both the ledger entry
    /// and the mutex on drop.
    async fn acquire_transition(&self, fid: u64) -> Result<TransitionLease, CordisError> {
        // Same-thread reentrancy pre-check: a live ledger entry for
        // (this thread, fid) means an ancestor transition on this fiber is
        // executing in this very call stack. Waiting could never succeed on a
        // non-reentrant mutex, so fail immediately instead of burning the
        // whole [`transition_wait()`] on ourselves.
        if HELD_TRANSITIONS
            .lock()
            .contains(&(std::thread::current().id(), fid))
        {
            return Err(CordisError::Fiber(format!(
                "reentrant transition on fiber {fid}"
            )));
        }
        let lock = match Arc::clone(&self.inertia).try_lock_owned() {
            Ok(lock) => lock,
            Err(_) => {
                match tokio::time::timeout(
                    transition_wait(),
                    Arc::clone(&self.inertia).lock_owned(),
                )
                .await
                {
                    Ok(lock) => lock,
                    Err(_elapsed) => {
                        let ms = transition_wait().as_millis();
                        tracing::error!("fiber {fid} stuck in transition over {ms}ms");
                        return Err(CordisError::Fiber(format!(
                            "fiber {fid} stuck in transition over {ms}ms"
                        )));
                    }
                }
            }
        };
        // Registered only once the guard is owned; the insert cannot collide
        // because the pre-check above ran on this same thread and the entry
        // is removed before the mutex is ever released (drop order).
        let ledger = TransitionGuard::acquire(fid)?;
        Ok(TransitionLease {
            _ledger: ledger,
            _lock: lock,
        })
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
        while let Some((_, undo)) = { self.acc.lock().pop() } {
            undo();
        }
    }

    /// Dispose this fiber: mark it disposed, undo its effects LIFO, and rest
    /// it as pristine `Inactive`. The inertia guard is acquired through
    /// [`Self::acquire_transition`] (bounded wait, same-thread reentrancy
    /// detection), so dispose can never park forever behind a hung
    /// transition — it instead returns the named-fiber error to the caller.
    pub async fn dispose(&self) -> Result<(), CordisError> {
        let fid = *self.id.lock().get_or_insert(0);
        let _guard = self.acquire_transition(fid).await?;
        self.disposed.store(true, Ordering::Release);
        self.set_state(FiberState::Unloading { error: None });
        while let Some((_, undo)) = { self.acc.lock().pop() } {
            undo();
        }
        self.set_state(FiberState::Inactive { error: None });
        self.set_epoch(String::new());
        Ok(())
    }

    /// Apply a config change through the same dependency reload runner used by
    /// reactive refresh. Existing registration effects are undone before the
    /// plugin is applied again.
    pub async fn update(&self, ctx: &Arc<Context>) {
        self.refresh(ctx).await;
    }

    // Called by Context::provide to push undo onto this fiber's acc under a
    // default "unnamed" label.
    pub(crate) fn push_undo(&self, undo: Box<dyn FnOnce() + Send>) {
        self.push_undo_labeled(UndoMeta::default(), undo);
    }

    /// Push an undo closure carrying explicit introspection metadata.
    pub fn push_undo_labeled(&self, meta: UndoMeta, undo: Box<dyn FnOnce() + Send>) {
        self.acc.lock().push((meta, undo));
    }

    /// Snapshot of the pending undo labels in registration (FIFO) order.
    ///
    /// Execution order is the reverse (LIFO); see [`Self::dispose`].
    pub fn pending_undo_labels(&self) -> Vec<String> {
        self.acc
            .lock()
            .iter()
            .map(|(meta, _)| meta.label.clone())
            .collect()
    }

    /// True once [`Self::dispose`] has run on this fiber. Disposed fibers are
    /// prunable from tracking maps; `Failed` fibers are not disposed and stay
    /// inspectable by design.
    pub fn is_disposed(&self) -> bool {
        self.disposed.load(Ordering::Acquire)
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
        let _ = fiber.dispose().await;
        let during = snap.lock().clone();
        assert_eq!(during, Some(FiberState::Unloading { error: None }));
        assert_eq!(fiber.state(), FiberState::Inactive { error: None });
    }

    /// Undo introspection: three labeled undos report their labels in
    /// registration (FIFO) order via `pending_undo_labels`; the default
    /// label for the historical `push_undo` path is "unnamed".
    #[test]
    fn pending_undo_labels_reports_registration_order() {
        let fiber = Fiber::new();
        assert!(fiber.pending_undo_labels().is_empty());
        fiber.push_undo_labeled(UndoMeta::new("provide:events"), Box::new(|| {}));
        fiber.push_undo(Box::new(|| {}));
        fiber.push_undo_labeled(UndoMeta::new("provide:store"), Box::new(|| {}));

        let labels = fiber.pending_undo_labels();
        assert_eq!(
            labels,
            vec![
                "provide:events".to_string(),
                "unnamed".to_string(),
                "provide:store".to_string()
            ]
        );
    }

    /// Dispose still runs every undo exactly once, in reverse registration
    /// (LIFO) order — the pre-existing contract, now over labeled entries.
    #[tokio::test]
    async fn dispose_runs_labeled_undos_in_lifo_order() {
        let fiber = Arc::new(Fiber::new());
        let ran = Arc::new(ParkingMutex::new(Vec::new()));
        for name in ["first", "second", "third"] {
            let r = ran.clone();
            fiber.push_undo_labeled(
                UndoMeta::new(name),
                Box::new(move || {
                    r.lock().push(name.to_string());
                }),
            );
        }
        // Introspection works while undos are still pending.
        assert_eq!(
            fiber.pending_undo_labels(),
            vec!["first", "second", "third"]
        );
        let _ = fiber.dispose().await;
        assert_eq!(*ran.lock(), ["third", "second", "first"]);
        // The accumulator drained; nothing left to introspect.
        assert!(fiber.pending_undo_labels().is_empty());
        assert!(fiber.is_disposed());
    }

    // ------------------------------------------------------------------
    // Bounded transitions: reentrancy vs contention classification
    // ------------------------------------------------------------------

    fn set_test_transition_wait(wait: Duration) {
        // Tests run multi-threaded in ONE process: whichever test sets first
        // wins, and both bounds satisfy every assertion here (< 1s).
        let _ = TRANSITION_WAIT_OVERRIDE.set(wait);
    }

    /// A transition attempted from within a live transition on the SAME
    /// thread (the recursive-deadlock shape of our sync-callback model) must
    /// be classified immediately as reentrant — no waiting on a mutex that
    /// can never free up.
    #[tokio::test]
    async fn reentrant_transition_detected_fast() {
        set_test_transition_wait(Duration::from_millis(50));
        let ctx = Context::new_root();
        let fiber = Arc::new(Fiber::new());
        fiber.set_reload_context(&ctx);
        fiber.set_id(90_001);

        // Simulate the ancestor transition holding the ledger slot.
        let _ancestor = TransitionGuard::acquire(90_001).unwrap();

        let start = std::time::Instant::now();
        // Refresh swallows acquisition failures into state changes; probe
        // both public paths and require the reentrant classification fast.
        let dispose_err = fiber.dispose().await.unwrap_err();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "reentrant detection must not wait out the budget, took {elapsed:?}"
        );
        match dispose_err {
            CordisError::Fiber(msg) => {
                assert!(
                    msg.contains("reentrant") && msg.contains("90001"),
                    "expected reentrant naming the fiber, got: {msg}"
                );
            }
            other => panic!("expected CordisError::Fiber, got {other:?}"),
        }
        fiber.refresh(&ctx).await;
        match fiber.state() {
            // Refresh cannot return the error; it rests untouched because
            // the reentrant attempt happens before any transition runs.
            FiberState::Inactive { error: None } => {}
            other => panic!("unexpected refresh state under reentrancy: {other:?}"),
        }
    }

    /// Cross-task contention is bounded: when the holder never yields, the
    /// waiter gives up after [`TRANSITION_WAIT`] with an error naming the
    /// stuck fiber id instead of parking forever.
    #[tokio::test]
    async fn contention_times_out_named() {
        set_test_transition_wait(Duration::from_millis(100));
        let ctx = Context::new_root();
        let fiber = Arc::new(Fiber::new());
        fiber.set_reload_context(&ctx);
        fiber.set_id(90_002);

        // Hold the inertia guard from another task until we say otherwise.
        let inertia = Arc::clone(&fiber.inertia);
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let releaser = release.clone();
        let holder = tokio::spawn(async move {
            let _guard = inertia.lock_owned().await;
            while !releaser.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        // Let the holder win the race for the guard.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let start = std::time::Instant::now();
        fiber.refresh(&ctx).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "contended refresh must time out fast, took {elapsed:?}"
        );
        let dispose_err = fiber.dispose().await.unwrap_err();
        match dispose_err {
            CordisError::Fiber(msg) => {
                assert!(
                    msg.contains("stuck") && msg.contains("90002") && msg.contains("ms"),
                    "expected stuck-in-transition naming the fiber, got: {msg}"
                );
            }
            other => panic!("expected CordisError::Fiber, got {other:?}"),
        }

        release.store(true, Ordering::Release);
        holder.await.unwrap();
    }

    /// [`Fiber::is_idle`] mirrors the inertia guard without blocking.
    #[tokio::test]
    async fn is_idle_reflects_lock_state() {
        let fiber = Arc::new(Fiber::new());
        assert!(fiber.is_idle(), "free guard must report idle");
        let held = Arc::clone(&fiber.inertia)
            .try_lock_owned()
            .expect("guard should be free");
        assert!(!fiber.is_idle(), "held guard must not report idle");
        drop(held);
        assert!(fiber.is_idle(), "released guard must report idle again");
    }

    /// [`Fiber::wait_idle`] resolves `true` on a free guard, waits out the
    /// budget behind a holder and reports `false`, and flips back to `true`
    /// once the holder releases before the budget expires.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_idle_reflects_lock_state_with_budget() {
        set_test_transition_wait(Duration::from_millis(100));
        let fiber = Arc::new(Fiber::new());

        // Free guard: immediate true.
        assert!(
            fiber.wait_idle().await,
            "free guard must resolve idle immediately"
        );

        // Held guard released BEFORE the budget: waiter observes the release
        // and still answers true.
        let held = Arc::clone(&fiber.inertia)
            .try_lock_owned()
            .expect("guard should be free");
        let waiter = tokio::spawn({
            let fiber = fiber.clone();
            async move { fiber.wait_idle().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(held);
        assert!(
            waiter.await.expect("waiter task"),
            "release inside the budget must answer true"
        );

        // Held guard NEVER released: bounded false after the budget.
        let stuck = Arc::clone(&fiber.inertia)
            .try_lock_owned()
            .expect("guard should be free again");
        let start = std::time::Instant::now();
        let busy = fiber.wait_idle().await;
        let elapsed = start.elapsed();
        drop(stuck);
        assert!(!busy, "never-released guard must answer false");
        assert!(
            elapsed >= Duration::from_millis(90) && elapsed < Duration::from_secs(1),
            "false must arrive only after the budget, took {elapsed:?}"
        );
    }

    /// [`Fiber::error`] surfaces the message carried by resting terminal
    /// states and stays `None` for healthy/in-flight states.
    #[test]
    fn error_accessor_reads_failed_state() {
        let fiber = Fiber::new();
        assert!(
            fiber.error().is_none(),
            "fresh Inactive with no error reports no error"
        );

        fiber.set_state(FiberState::Active { epoch: ":".into() });
        assert_eq!(fiber.error(), None);

        fiber.set_state(FiberState::Loading);
        assert_eq!(fiber.error(), None);

        fiber.set_state(FiberState::Reloading);
        assert_eq!(fiber.error(), None);

        fiber.set_state(FiberState::Failed {
            error: Some("plugin apply blew up".into()),
        });
        assert_eq!(
            fiber.error().as_deref(),
            Some("plugin apply blew up"),
            "Failed error message must surface"
        );

        fiber.set_state(FiberState::Inactive {
            error: Some("missing or inactive dependency".into()),
        });
        assert_eq!(
            fiber.error(),
            Some("missing or inactive dependency".to_string())
        );

        fiber.set_state(FiberState::Unloading {
            error: Some("dispose undo panicked".into()),
        });
        assert_eq!(fiber.error(), Some("dispose undo panicked".to_string()));
    }
}
