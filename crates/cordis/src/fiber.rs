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
            id: Mutex::new(None),
            disposed: AtomicBool::new(false),
        }
    }

    pub fn declare_inject<T: Service>(&self) {
        let tid = TypeId::of::<T>();
        let name = std::any::type_name::<T>().to_string();
        self.injects.write().insert(tid, name);
        if let (Some(ctx), Some(fid)) = (
            self.reload_ctx
                .lock()
                .as_ref()
                .and_then(std::sync::Weak::upgrade),
            *self.id.lock(),
        ) {
            if let Some(reflect) = ctx.get::<crate::ReflectService>() {
                reflect.register_dependent(tid, fid);
                let _ = reflect.ensure_notifier(tid);
            }
        }
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

    /// Refresh recomputes dependency epoch and reruns registered plugins.
    pub async fn refresh(&self, ctx: &Arc<Context>) {
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
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
        if new_epoch == old_epoch && satisfied && matches!(previous, FiberState::Active { .. }) {
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
    }

    fn is_satisfied(&self, ctx: &Arc<Context>) -> bool {
        self.injects.read().keys().all(|tid| ctx.is_available(*tid))
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
