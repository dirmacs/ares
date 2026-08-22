//! Adapter from [`AgentExecutionService`](ares_agents::execution::AgentExecutionService)
//! to [`ares_mcp::AgentRunner`].

use std::sync::Arc;

use ares_cordis_core::Context;

/// Runs MCP `ares_run_agent` through Cordis `AgentExecutionService`.
pub struct ExecutionAgentRunner {
    pub ctx: Arc<Context>,
}

#[async_trait::async_trait]
impl ares_mcp::AgentRunner for ExecutionAgentRunner {
    async fn run_agent(
        &self,
        input: &ares_mcp::tools::RunAgentInput,
    ) -> Result<ares_mcp::tools::RunAgentOutput, String> {
        let exec = self
            .ctx
            .get::<ares_agents::execution::AgentExecutionService>()
            .ok_or_else(|| "AgentExecutionService not provided".to_string())?;
        let req = ares_agents::execution::AgentRequest {
            agent_name: input.agent_name.clone(),
            message: input.message.clone(),
            tenant: None,
            history: vec![],
            ctx_provider: None,
        };
        let resp = exec
            .execute(req, &self.ctx)
            .await
            .map_err(|e| e.to_string())?;
        Ok(ares_mcp::tools::RunAgentOutput {
            response: resp.content,
            agent: input.agent_name.clone(),
            context_id: input.context_id.clone().unwrap_or_default(),
            sources: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ExecutionAgentRunner;
    use ares_mcp::AgentRunner;

    #[tokio::test]
    async fn execution_agent_runner_errors_without_service() {
        let ctx = ares_cordis_core::Context::new_root();
        let runner = ExecutionAgentRunner { ctx };
        let err = runner
            .run_agent(&ares_mcp::tools::RunAgentInput {
                agent_name: "router".into(),
                message: "hi".into(),
                context_id: None,
            })
            .await
            .unwrap_err();
        assert!(err.contains("AgentExecutionService"));
    }
}
