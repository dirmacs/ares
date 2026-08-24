//! Per-tenant Cordis child contexts.
//!
//! `TypeId`s for Tools and Execute are supplied by the caller so this crate
//! never depends on `ares-tools` or `ares-agent`.

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;

use cordis::{Context, Service};
use parking_lot::RwLock;

/// Cached child contexts, one fiber per tenant id.
pub struct TenantRealms {
    realms: RwLock<HashMap<String, Arc<Context>>>,
    tools: TypeId,
    execute: TypeId,
}

impl TenantRealms {
    pub fn new(tools: TypeId, execute: TypeId) -> Self {
        Self {
            realms: RwLock::new(HashMap::new()),
            tools,
            execute,
        }
    }

    /// Return the cached child for `tenant_id`, creating it on first open.
    ///
    /// The child is `root.extend()` isolated on the Tools TypeId only: tools
    /// carry per-tenant data, while `Execute` is a stateless shared engine that
    /// must stay resolvable from inside the realm (its tenancy comes from the
    /// context it is handed). No `TenantContext` intercept is applied.
    pub fn open(&self, root: &Arc<Context>, tenant_id: &str) -> Arc<Context> {
        if let Some(existing) = self.realms.read().get(tenant_id) {
            return existing.clone();
        }
        let mut map = self.realms.write();
        if let Some(existing) = map.get(tenant_id) {
            return existing.clone();
        }
        let _ = self.execute;
        let child = root.extend().isolate_type(self.tools, tenant_id);
        map.insert(tenant_id.to_string(), child.clone());
        child
    }

    /// Drop the cached child and dispose its fiber (LIFO undoes that realm's provides).
    pub async fn dispose(&self, tenant_id: &str) {
        let child = self.realms.write().remove(tenant_id);
        if let Some(child) = child {
            if let Err(error) = child.fiber().dispose().await {
                tracing::error!(tenant_id, %error, "TenantRealms: fiber stuck in transition during dispose");
            }
        }
    }
}

impl Service for TenantRealms {
    fn name(&self) -> &'static str {
        "TenantRealms"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordis::Context;

    struct DummyTools;
    impl Service for DummyTools {}

    struct DummyExecute;
    impl Service for DummyExecute {}

    struct ChildOnly;
    impl Service for ChildOnly {}

    #[tokio::test]
    async fn tenant_realm_dispose_drops_isolated_tools() {
        let root = Context::new_root();
        root.provide(DummyTools);
        let realms = TenantRealms::new(TypeId::of::<DummyTools>(), TypeId::of::<DummyExecute>());
        let child = realms.open(&root, "acme");
        child.provide(ChildOnly);
        assert!(child.get::<ChildOnly>().is_some());
        assert!(root.get::<DummyTools>().is_some());

        realms.dispose("acme").await;

        assert!(
            child.get::<ChildOnly>().is_none(),
            "child-only provide must undo on fiber dispose"
        );
        assert!(
            root.get::<DummyTools>().is_some(),
            "root DummyTools must survive realm dispose"
        );
    }
}
