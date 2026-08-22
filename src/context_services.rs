//! Remaining Cordis context types that live in the server crate:
//! - [`EmergencyStop`] — native Service (kill switch)
//!
//! Tenant tool isolation uses `ares_agent::tenant_scope(ctx, tenant_id)` (`Tools` + `Execute`).
//! `PostgresClient` and [`ares_agent::ContextProviderHandle`] are native Service
//! types; handlers `ctx.get` them directly.

use crate::AppState;
use std::sync::atomic::AtomicBool;

use cordis::Service;

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
    fn init(&self, _ctx: &AppState) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

// ContextProviderHandle (was ContextProviderService) lives in ares_agent::context_provider.

#[cfg(test)]
mod tests {
    use super::*;
    use cordis::Context;

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
