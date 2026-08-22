use parking_lot::{Mutex, RwLock};
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;

use crate::context::Context;
use crate::service::Service;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FiberState {
    Inactive { error: Option<String> },
    Active { epoch: String },
    /// A plugin activation is in flight (Cordis `LOADING`).
    Loading,
    /// Activation finished with an error; the plugin is not serving (Cordis `FAILED`).
    Failed { error: Option<String> },
    Reloading,
    Unloading { error: Option<String> },
}

pub struct Fiber {
    state: RwLock<FiberState>,
    inertia: Arc<tokio::sync::Mutex<()>>,
    acc: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    epoch: RwLock<String>,
    injects: RwLock<HashMap<TypeId, String>>, // TypeId -> type_name
    // committed snapshot placeholder
}

impl Fiber {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(FiberState::Inactive { error: None }),
            inertia: Arc::new(tokio::sync::Mutex::new(())),
            acc: Mutex::new(Vec::new()),
            epoch: RwLock::new(String::new()),
            injects: RwLock::new(HashMap::new()),
        }
    }

    pub fn declare_inject<T: Service>(&self) {
        let tid = TypeId::of::<T>();
        let name = std::any::type_name::<T>().to_string();
        self.injects.write().insert(tid, name);
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

    /// Refresh recomputes epoch from inject deps; if changed, reload.
    pub async fn refresh(&self, ctx: &Arc<Context>) {
        let _guard = self.inertia.lock().await;
        let new_epoch = self.compute_epoch(ctx);
        let old_epoch = self.epoch.read().clone();
        let prev = self.state.read().clone();
        if new_epoch == old_epoch && self.state() != (FiberState::Inactive { error: None }) {
            // No change and already active -> no reload
            // But also check if still active vs inactive due to dep satisfaction
            let satisfied = self.is_satisfied(ctx);
            if satisfied && matches!(self.state(), FiberState::Inactive { .. }) {
                // was inactive but now satisfied without epoch change? Should activate
                // e.g., first provide when epoch goes from "" to ":Foo:1"
            } else {
                return;
            }
        }
        self.set_state(FiberState::Reloading);
        tokio::task::yield_now().await;
        let satisfied = self.is_satisfied(ctx);
        if satisfied {
            self.set_epoch(new_epoch.clone());
            self.set_state(FiberState::Active {
                epoch: new_epoch.clone(),
            });
            if prev != self.state() {
                tracing::info!(from=?prev, to=?self.state(), epoch=%new_epoch, "Cordis fiber transition");
            }
        } else {
            self.set_state(FiberState::Inactive { error: None });
            if prev != self.state() {
                tracing::info!(from=?prev, to=?self.state(), epoch=%new_epoch, "Cordis fiber transition");
            }
            // do not update epoch when inactive? keep old?
        }
    }

    fn is_satisfied(&self, ctx: &Arc<Context>) -> bool {
        let injects = self.injects.read();
        for tid in injects.keys() {
            if ctx.get_version(*tid) == 0 {
                return false;
            }
        }
        true
    }

    pub async fn dispose(&self) {
        let _guard = self.inertia.lock().await;
        self.set_state(FiberState::Unloading { error: None });
        let mut acc = self.acc.lock();
        while let Some(undo) = acc.pop() {
            undo();
        }
        self.set_state(FiberState::Inactive { error: None });
        self.set_epoch(String::new());
    }

    /// Apply a config change to a live fiber by recomputing its epoch and
    /// re-running the dependency-satisfaction check against `ctx`.  This is the
    /// synchronization point the Loader calls from its `UpdateConfig` arm; it
    /// is additive and non-destructive (unlike `dispose`), so a config-only
    /// change keeps the fiber's accumulator and committed view intact.
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
