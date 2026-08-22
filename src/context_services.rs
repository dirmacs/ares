//! Remaining Cordis wrappers that cannot impl Service on the inner type:
//! - [`ToolRegistryService`] — isolate TypeId for tenant tool realms
//! - [`EmergencyStop`] — native Service (kill switch)
//!
//! `PostgresClient` and [`ares_agents::ContextProviderHandle`] are native Service
//! types; handlers `ctx.get` them directly.

use std::sync::Arc;
use crate::AppState;
use std::sync::atomic::AtomicBool;

use crate::ToolRegistry;
use ares_cordis_core::Service;

// LLM — ConfigBasedLLMFactory now implements Service directly (ctx.get::<ConfigBasedLLMFactory>())
// AgentRegistry now implements Service directly (ctx.get::<AgentRegistry>())

pub struct ToolRegistryService(pub Arc<ToolRegistry>);
impl Service for ToolRegistryService {}

// Emergency stop
/// Global agent-execution kill switch.
pub struct EmergencyStop {
    flag: AtomicBool,
}

impl EmergencyStop {
    pub fn new(active: bool) -> Self {
        Self {
            flag: AtomicBool::new(active),
        }
    }

    pub fn is_active(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_active(&self, active: bool) {
        self.flag
            .store(active, std::sync::atomic::Ordering::Relaxed)
    }
}

impl Service for EmergencyStop {
    fn name(&self) -> &'static str {
        "emergency_stop"
    }
    fn init(&self, _ctx: &AppState) -> ares_cordis_core::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

// ContextProviderHandle (was ContextProviderService) lives in ares_agents::context_provider.

#[cfg(test)]
mod tests {
    use super::*;
    use ares_cordis_core::Context;

    #[test]
    fn emergency_stop_readable_via_cordis() {
        let ctx = Context::new_root();
        ctx.provide(EmergencyStop::new(false));
        let got = ctx.get::<EmergencyStop>().expect("provided");
        assert!(!got.is_active());
        got.set_active(true);
        assert!(got.is_active());
    }

    #[tokio::test]
    async fn postgres_client_readable_via_cordis() {
        let ctx = Context::new_root();
        let client = crate::db::PostgresClient::new_test();
        ctx.provide(client);
        assert!(ctx.get::<crate::db::PostgresClient>().is_some());
    }
}
