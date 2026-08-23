//! Library proof: `Execute::run` with no HTTP / axum on the graph.

use std::sync::Arc;

use ares::{
    AgentRequest, AppError, Calculator, Context, ConversationMessage, Execute, Llm, LLMClient,
    LLMResponse, PluginRegistry, TenantContext, TenantTier, Tool, ToolDefinition, Tools,
};
use async_trait::async_trait;

/// Minimal in-test LLM: never performs network I/O.
struct MockLLMClient {
    model: String,
}

impl MockLLMClient {
    fn new(model: impl Into<String>) -> Self {
        Self { model: model.into() }
    }
}

#[async_trait]
impl LLMClient for MockLLMClient {
    async fn generate(&self, _prompt: &str) -> Result<String, AppError> {
        Ok("pong".into())
    }

    async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String, AppError> {
        Ok("pong".into())
    }

    async fn generate_with_history(
        &self,
        _messages: &[(String, String)],
    ) -> Result<LLMResponse, AppError> {
        Ok(LLMResponse {
            content: "pong".into(),
            tool_calls: vec![],
            finish_reason: "stop".into(),
            usage: None,
        })
    }

    async fn generate_with_tools(
        &self,
        _prompt: &str,
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse, AppError> {
        Ok(LLMResponse {
            content: "pong".into(),
            tool_calls: vec![],
            finish_reason: "stop".into(),
            usage: None,
        })
    }

    async fn generate_with_tools_and_history(
        &self,
        _messages: &[ConversationMessage],
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse, AppError> {
        Ok(LLMResponse {
            content: "pong".into(),
            tool_calls: vec![],
            finish_reason: "stop".into(),
            usage: None,
        })
    }

    async fn stream(
        &self,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String, AppError>> + Send + Unpin>, AppError>
    {
        Err(AppError::Internal("mock stream not implemented".into()))
    }

    async fn stream_with_system(
        &self,
        _system: &str,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String, AppError>> + Send + Unpin>, AppError>
    {
        Err(AppError::Internal("mock stream not implemented".into()))
    }

    async fn stream_with_history(
        &self,
        _messages: &[(String, String)],
    ) -> Result<Box<dyn futures::Stream<Item = Result<String, AppError>> + Send + Unpin>, AppError>
    {
        Err(AppError::Internal("mock stream not implemented".into()))
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

fn static_tools() -> Tools {
    Tools::from_static([Arc::new(Calculator) as Arc<dyn Tool>])
}

#[tokio::test]
async fn execute_runs_without_http() {
    let mock = Arc::new(MockLLMClient::new("mock"));

    let reg = PluginRegistry::new();
    ares::register_plugins(&reg);

    let ctx = Context::new_root().with_intercept(TenantContext::new(
        "lib".into(),
        TenantTier::Pro,
    ));

    ctx.provide(static_tools());
    ctx.provide(Llm::from_client(mock));
    let execute = ctx.provide(Execute::new());

    let req = AgentRequest {
        agent_name: "ping".into(),
        message: "ping".into(),
        history: vec![],
        ctx_provider: None,
    };
    let result = execute
        .run(&req, &ctx)
        .await
        .expect("Execute::run without HTTP");
    assert!(
        !result.response.content.is_empty(),
        "expected non-empty content, got {:?}",
        result.response.content
    );
}
