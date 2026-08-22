//! Library proof: `Execute::run` with no HTTP / axum on the graph.

use std::collections::HashMap;
use std::sync::Arc;

use ares::{
    AgentConfig, AgentRegistry, AgentRequest, AppError, Calculator, ClientPool, Context,
    ConversationMessage, Execute, Llm, LLMClient, LLMResponse, PluginRegistry, ProviderRegistry,
    TenantContext, TenantTier, Tool, ToolDefinition, Tools,
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

fn ping_agent() -> AgentConfig {
    AgentConfig {
        model: "mock".into(),
        system_prompt: Some("You are ping.".into()),
        tools: vec!["calculator".into()],
        allowed_tools: None,
        max_tool_iterations: 10,
        parallel_tools: false,
        extra: HashMap::new(),
    }
}

fn static_tools() -> Tools {
    Tools::from_static([Arc::new(Calculator) as Arc<dyn Tool>])
}

#[tokio::test]
async fn execute_runs_without_http() {
    let _mock = MockLLMClient::new("mock");

    let reg = PluginRegistry::new();
    ares::register_plugins(&reg);

    let ctx = Context::new_root().with_intercept(TenantContext::new(
        "lib".into(),
        TenantTier::Pro,
    ));

    let tools = static_tools();
    ctx.provide(tools);

    let providers = Arc::new(ProviderRegistry::new());
    let llm = Llm::new(Arc::clone(&providers), Arc::new(ClientPool::default()), None);
    ctx.provide(llm);

    let mut agents = HashMap::new();
    agents.insert("ping".into(), ping_agent());
    let registry = Arc::new(AgentRegistry::from_config(
        agents,
        providers,
        Arc::new(static_tools()),
    ));
    let execute = ctx.provide(Execute::new().with_agent_registry(registry));

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
