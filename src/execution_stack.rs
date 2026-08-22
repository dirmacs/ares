//! Shared agent-execution stack for HTTP boot and MCP in-process runner.
use std::sync::Arc;
use ares_cordis_core::Context;
use ares_agents::execution::AgentExecutionService;

/// Provide `service` on `root_ctx` and return it (single-source execution spine).
pub fn provide_shared_execution(
    root_ctx: &Arc<Context>,
    service: Arc<AgentExecutionService>,
) -> Arc<AgentExecutionService> {
    root_ctx.provide_arc(service.clone());
    service
}

#[cfg(feature = "postgres")]
pub fn new_shared_execution(
    db: Arc<dyn ares_db::traits::DatabaseClient>,
    tenant_db: Arc<crate::TenantDb>,
    llm_factory: Arc<ares_llm::provider_registry::ConfigBasedLLMFactory>,
    agent_registry: Arc<ares_agents::AgentRegistry>,
    active_runs: Arc<dyn ares_agents::RunTracker>,
) -> Arc<AgentExecutionService> {
    Arc::new(
        AgentExecutionService::new()
            .with_db(db)
            .with_tenant_db(tenant_db)
            .with_llm_factory(llm_factory)
            .with_agent_registry(agent_registry)
            .with_fleet_secrets(Arc::new(crate::FleetSecrets::new()))
            .with_run_tracker(active_runs),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provide_shared_execution_puts_service_on_context() {
        let ctx = Context::new_root();
        let svc = Arc::new(AgentExecutionService::new());
        provide_shared_execution(&ctx, svc);
        assert!(ctx.get::<AgentExecutionService>().is_some());
    }
}
