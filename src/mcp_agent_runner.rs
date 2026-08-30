//! Adapter from [`Execute`](ares_agent::execution::Execute)
//! to [`ares_mcp::AgentRunner`].

use std::sync::Arc;

use cordis::Context;

/// Runs MCP `ares_run_agent` through Cordis `Execute`.
pub struct ExecutionAgentRunner {
    pub ctx: Arc<Context>,
}

impl ExecutionAgentRunner {
    /// Context already holding Execute.
    pub fn new(ctx: Arc<Context>) -> Self {
        Self { ctx }
    }

    /// Provide Execute::new() if missing, then wrap.
    pub fn attach(ctx: Arc<Context>) -> Self {
        if ctx.get::<ares_agent::execution::Execute>().is_none() {
            ctx.provide_arc(Arc::new(ares_agent::execution::Execute::new()));
        }
        Self { ctx }
    }

    /// Build a root context, attach tenant DB, provide the execution service.
    #[cfg(feature = "postgres")]
    pub fn with_tenant_db(tenant_db: Arc<ares_store::TenantDb>) -> Self {
        let ctx = Context::new_root();
        ctx.provide_arc(tenant_db);
        ctx.provide_arc(Arc::new(ares_agent::execution::Execute::new()));
        Self { ctx }
    }

    /// Context used by this runner (for tests and MCP stdio wiring).
    pub fn context(&self) -> &Arc<Context> {
        &self.ctx
    }
}

#[async_trait::async_trait]
impl ares_mcp::AgentRunner for ExecutionAgentRunner {
    async fn run_agent(
        &self,
        input: &ares_mcp::tools::RunAgentInput,
    ) -> Result<ares_mcp::tools::RunAgentOutput, String> {
        let exec = self
            .ctx
            .get::<ares_agent::execution::Execute>()
            .ok_or_else(|| "Execute not provided".to_string())?;
        let req = ares_agent::execution::AgentRequest {
            agent_name: input.agent_name.clone(),
            message: input.message.clone(),
            history: vec![],
            ctx_provider: None,
            ..Default::default()
        };
        let exec_result = exec.run(&req, &self.ctx).await.map_err(|e| e.to_string())?;
        Ok(ares_mcp::tools::RunAgentOutput {
            response: exec_result.response.content,
            agent: input.agent_name.clone(),
            context_id: input.context_id.clone().unwrap_or_default(),
            sources: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ExecutionAgentRunner;
    use std::sync::Arc;

    use ares_mcp::AgentRunner;

    #[tokio::test]
    async fn execution_agent_runner_errors_without_service() {
        let ctx = cordis::Context::new_root();
        let runner = ExecutionAgentRunner { ctx };
        let err = runner
            .run_agent(&ares_mcp::tools::RunAgentInput {
                agent_name: "router".into(),
                message: "hi".into(),
                context_id: None,
            })
            .await
            .unwrap_err();
        assert!(err.contains("Execute"));
    }

    #[test]
    fn attach_provides_execution_service() {
        let runner = ExecutionAgentRunner::attach(cordis::Context::new_root());
        assert!(runner
            .context()
            .get::<ares_agent::execution::Execute>()
            .is_some());
    }

    #[test]
    fn attach_keeps_existing_service() {
        let ctx = cordis::Context::new_root();
        let existing = Arc::new(ares_agent::execution::Execute::new());
        ctx.provide_arc(existing.clone());
        let runner = ExecutionAgentRunner::attach(ctx);
        let got = runner
            .context()
            .get::<ares_agent::execution::Execute>()
            .expect("existing service kept");
        assert!(Arc::ptr_eq(&existing, &got));
    }
}
