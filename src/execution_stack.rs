//! Shared agent-execution stack for HTTP boot and MCP in-process runner.
use std::sync::Arc;
use cordis::Context;
use ares_agent::execution::Execute;

/// Provide `service` on `root_ctx` and return it (single-source execution spine).
pub fn provide_shared_execution(
    root_ctx: &Arc<Context>,
    service: Arc<Execute>,
) -> Arc<Execute> {
    root_ctx.provide_arc(service.clone());
    service
}

#[cfg(feature = "postgres")]
pub fn new_shared_execution(
    agent_registry: Arc<ares_agent::AgentRegistry>,
    active_runs: Arc<dyn ares_agent::RunTracker>,
) -> Arc<Execute> {
    Arc::new(
        Execute::new()
            .with_agent_registry(agent_registry)
            .with_run_tracker(active_runs),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provide_shared_execution_puts_service_on_context() {
        let ctx = Context::new_root();
        let svc = Arc::new(Execute::new());
        provide_shared_execution(&ctx, svc);
        assert!(ctx.get::<Execute>().is_some());
    }
}
