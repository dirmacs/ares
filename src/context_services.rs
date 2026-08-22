//! Cordis Context service wrappers for legacy AppState fields.
//! Each wrapper holds the original AppState field type and implements `Service`
//! so `ctx.get::<Wrapper>().unwrap().0.clone()` retrieves the field via Context.

use std::sync::Arc;
use crate::AppState;
use std::sync::atomic::AtomicBool;

use crate::agents::context_provider::ContextProvider;
use crate::db::traits::DatabaseClient;
use crate::ToolRegistry;
use ares_cordis_core::Service;

// Trait-object wrappers: the inner types are `dyn` and cannot implement Service themselves.
pub struct DbService(pub Arc<dyn DatabaseClient>);
impl Service for DbService {}

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

// Context provider
pub struct ContextProviderService(pub Arc<dyn ContextProvider>);
impl Service for ContextProviderService {}


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
}
