//! V1 chat domain — cordis Phase6
//! Bodies moved from v1.rs

use std::sync::Arc;
use cordis::Context;
use super::*;

use ares_agent::context_provider::AgentRuntimeContext;
use ares_store::agent_runs;
use ares_types::models::TenantContext;
use ares_agent::research::coordinator::ResearchCoordinator;
use ares_types::types::{AgentContext, AgentType, AppError, ChatRequest, ChatResponse, ResearchRequest, ResearchResponse};
use crate::Result;
use crate::HttpError;
use axum::{
    extract::{Extension, State},
    response::Response,
    Json,
};

/// POST /v1/chat — tenant-scoped chat (API key auth, no conversation history)
pub async fn v1_chat(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Json(payload): Json<ChatRequest>,
) -> Result<axum::response::Response> {
    let tc = extract_tenant(ctx)?;
    // Open the tenant realm when TenantRealms is on ctx, then intercept TenantContext.
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    // Per-request model override is implemented via ModelOverride + Cordis intercept
    // (see the DI path below: state_ctx.with_intercept(ModelOverride { .. })).

    // Emergency stop — kill switch for all agents
    if state_ctx.get::<ares_agent::EmergencyStop>().expect("not provided")
        .is_active()
    {
        return Err(HttpError::from(ares_types::types::AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        )));
    }

    // Build a minimal agent context (no user-level conversation/memory)
    let agent_context = AgentContext {
        user_id: tc.tenant_id.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        conversation_history: vec![],
        user_memory: None,
    };

    // Determine agent type
    let agent_type = if let Some(at) = payload.agent_type {
        at
    } else {
        AgentType::Orchestrator
    };

    // Execute agent with timing
    let agent_name = ares_agent::registry::AgentRegistry::type_to_name(&agent_type).to_string();
    let start = std::time::Instant::now();

    // Inject Eruka context — the core product feature.
    // Calls the ContextProvider (ErukaContextProvider in managed mode, NoOp in OSS)
    // to fetch per-agent knowledge state and gap constraints from Eruka.
    let mut runtime_context =
        AgentRuntimeContext::new(tc.tenant_id.clone(), agent_name.clone(), "api_v1_chat");
    runtime_context.workspace_id = payload.workspace_id.clone();
    runtime_context.session_id = Some(agent_context.session_id.clone());

    let eruka_context = state_ctx.get::<ares_agent::ContextProviderHandle>().expect("not provided").0
        .get_context_for_run(&runtime_context)
        .await;
    let eruka_context_hit = eruka_context.is_some();

    let effective_message = if let Some(ctx) = eruka_context.as_deref() {
        tracing::info!(
            agent = %agent_name,
            tenant = %tc.tenant_id,
            ctx_len = ctx.len(),
            "External context injected into agent call"
        );
        format_message_with_context(ctx, &payload.message)
    } else {
        payload.message.clone()
    };

    // Phase 4 §15: v1/chat delegates to Execute for resolve+create+execute.
    // Observability (agent_runs recording) remains local since it needs v1-specific metadata
    // (workspace_id, eruka_context_hit, request_source).
    if let Some(exec_svc) = state_ctx.get::<ares_agent::execution::Execute>() {
        let req = ares_agent::execution::AgentRequest {
            agent_name: agent_name.clone(),
            message: effective_message.clone(),
            history: agent_context.conversation_history.clone(),
            ctx_provider: None,
        };
        // Cordis intercepts are request-local and composable: pin the model,
        // then attach the tenant's current allowlist without mutating root state.
        let req_ctx = if let Some(m) = &payload.model {
            let pool = state_ctx
                .get::<ares_store::TenantDb>()
                .expect("not provided")
                .pool()
                .clone();
            let allowed_models = ares_store::tenant_allowlist::TenantAllowlistStore::new(&pool)
                .list_models(&tc.tenant_id)
                .await?;
            let child = state_ctx.with_intercept(ares_agent::execution::ModelOverride {
                model: m.clone(),
            });
            child.provide(ares_llm::TenantModelPolicy::new(
                tc.tenant_id.clone(),
                allowed_models.into_iter().map(|item| item.model_id),
            ));
            child
        } else {
            state_ctx.clone()
        };
        let req_ctx = ares_agent::tenant_scope(&req_ctx, &tc.tenant_id);
        if let Some(ext) = eruka_context.clone() {
            req_ctx.provide(ares_agent::ExternalContext(ext));
        }
        let exec_result = exec_svc.run(&req, &req_ctx).await?;
        let duration_ms = start.elapsed().as_millis() as i64;
        let response_text = exec_result.response.content;
        let (model_name, provider_name) = execution_metadata_names(exec_result.response.metadata.as_ref());
        let (input_tokens, output_tokens) =
            llm_token_counts_u32(exec_result.response.usage.as_ref(), &effective_message, &response_text);

        // Record agent run with metadata from ExecutionResult
        {
            let pool = state_ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
            let tid = tc.tenant_id.clone();
            let aname = exec_result.agent_name.clone();
            let itok = input_tokens as i64;
            let otok = output_tokens as i64;
            let mname = model_name.clone();
            let pname = provider_name.clone();
            let metadata = agent_runs::AgentRunMetadata {
                workspace_id: payload.workspace_id.clone(),
                session_id: Some(agent_context.session_id.clone()),
                request_source: Some("api_v1_chat".to_string()),
                product: None,
                agent_config_source: Some(exec_result.source.as_str().to_string()),
                agent_config_version: None,
                eruka_binding_id: None,
                eruka_context_hit,
                eruka_read_count: if eruka_context_hit { 1 } else { 0 },
                eruka_write_count: 0,
                pipeline_id: None,
                schedule_id: None,
                trigger_id: None,
            };
            tokio::spawn(async move {
                let _ = agent_runs::insert_agent_run_with_metadata(
                    &pool, &tid, &aname, None, "completed", itok, otok, duration_ms,
                    None, &mname, &pname, false, Some(&metadata),
                ).await;
            });
        }

        return Ok(axum::Json(serde_json::json!({
            "response": response_text,
            "agent": exec_result.agent_name,
            "source": exec_result.source.as_str(),
            "model": model_name,
            "provider": provider_name,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            }
        })).into_response());
    }

    Err(HttpError::from(AppError::Unavailable(
        "Execute is not provided on the request context".into()
    )))
}

async fn ensure_research_model_allowed(
    state_ctx: &Arc<Context>,
    tenant_id: &str,
    model_name: &str,
) -> Result<()> {
    let pool = state_ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let allowlist_store = ares_store::tenant_allowlist::TenantAllowlistStore::new(&pool);
    research_model_allowlist_decision(
        allowlist_store
            .is_model_allowed(tenant_id, model_name)
            .await?,
        model_name,
    )
}

pub(crate) fn research_model_allowlist_decision(is_allowed: bool, model_name: &str) -> Result<()> {
    if is_allowed {
        return Ok(());
    }
    Err(HttpError::from(AppError::Auth(format!(
        "Model '{}' is not allowed for this tenant",
        model_name
    ).into())))
}

/// POST /v1/research — tenant-scoped research with provider-reported metering.
pub async fn v1_research(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Json(payload): Json<ResearchRequest>,
) -> Result<Response> {
    let tc = extract_tenant(ctx)?;
    let state_ctx = ares_agent::request_tenant_ctx(&state_ctx, tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };

    if state_ctx.get::<ares_agent::EmergencyStop>().expect("not provided")
        .is_active()
    {
        return Err(HttpError::from(AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string()
        )));
    }

    let start = std::time::Instant::now();
    let config = state_ctx.get::<crate::overlay::AresConfigManager>().expect("not provided").config();
    let workflow = config.get_workflow("research");
    let (depth, max_iterations) = research_depth_and_iterations(
        payload.depth,
        payload.max_iterations,
        workflow.map(|w| w.max_depth),
        workflow.map(|w| w.max_iterations),
    );

    let model_key = config
        .get_agent("orchestrator")
        .map(|a| a.model.as_str())
        .unwrap_or("powerful");
    let configured_provider = config
        .get_model(model_key)
        .map(|m| m.provider.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let llm = state_ctx.get::<ares_llm::Llm>().ok_or_else(|| {
        HttpError::from(AppError::Configuration(
            "Llm service is not provided on the request context".to_string(),
        ))
    })?;
    let model_ctx = state_ctx.with_intercept(ares_llm::ModelOverride {
        model: model_key.to_string(),
    });
    let llm_client = llm
        .get_client_boxed(&model_ctx, ares_llm::CapabilityRequirements::default())
        .await?;
    let model_name = llm_client.model_name().to_string();
    ensure_research_model_allowed(&state_ctx, &tc.tenant_id, &model_name).await?;

    let coordinator = ResearchCoordinator::new(llm_client, depth, max_iterations);
    let (findings, sources, usage) = coordinator.research_with_usage(&payload.query).await?;

    let response = ResearchResponse {
        findings,
        sources,
        duration_ms: start.elapsed().as_millis() as u64,
    };

    Ok(usage_response(
        response,
        usage.input_tokens as u64,
        usage.output_tokens as u64,
        &model_name,
        &configured_provider,
        "research",
    ))
}

pub fn routes() -> axum::Router<Arc<Context>> {
    use axum::routing::post;
    axum::Router::new()
        .route("/v1/chat/v1_chat", post(v1_chat))
        .route("/v1/chat/v1_research", post(v1_research))
}

// cordis Phase6: RouteSet Service
use cordis::Service;

#[cfg(test)]
mod tests {
    use super::*;
    use ares_agent::ConfigurableAgent;
    use ares_agent::AgentConfig;
    use ares_llm::{LLMClient, LLMResponse};
    use ares_tools::Tool;
    use ares_types::types::ToolDefinition;
    use async_trait::async_trait;
    use serde_json::Value;

    struct TestTool(&'static str);

    #[async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            self.0
        }

        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _args: Value) -> ares_types::Result<Value> {
            Ok(serde_json::json!({"tool": self.0}))
        }
    }

    struct TestLlm;

    fn test_response() -> LLMResponse {
        LLMResponse {
            content: String::new(),
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            usage: None,
        }
    }

    #[async_trait]
    impl LLMClient for TestLlm {
        async fn generate(&self, _prompt: &str) -> ares_types::Result<String> {
            Ok(String::new())
        }

        async fn generate_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> ares_types::Result<String> {
            Ok(String::new())
        }

        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> ares_types::Result<LLMResponse> {
            Ok(test_response())
        }

        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ToolDefinition],
        ) -> ares_types::Result<LLMResponse> {
            Ok(test_response())
        }

        async fn generate_with_tools_and_history(
            &self,
            _messages: &[ares_llm::coordinator::ConversationMessage],
            _tools: &[ToolDefinition],
        ) -> ares_types::Result<LLMResponse> {
            Ok(test_response())
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> ares_types::Result<Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> ares_types::Result<Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> ares_types::Result<Box<dyn futures::Stream<Item = ares_types::Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }

        fn model_name(&self) -> &str {
            "test"
        }
    }

    #[test]
    fn tenant_isolated_service_reaches_agent_and_denies_other_tenant_tool() {
        let root = Context::new_root();
        let tools = ares_tools::Tools::from_static([
            Arc::new(TestTool("tenant_a_tool")) as Arc<dyn Tool>,
            Arc::new(TestTool("tenant_b_tool")) as Arc<dyn Tool>,
        ]);
        root.provide(tools);

        let config = AgentConfig {
            model: "test".to_string(),
            system_prompt: Some("test".to_string()),
            tools: vec!["tenant_a_tool".to_string()],
            allowed_tools: None,
            max_tool_iterations: 1,
            parallel_tools: false,
            extra: std::collections::HashMap::new(),
        };
        let mut agent = ConfigurableAgent::new_with_tool_service(
            "tenant-a",
            &config,
            Box::new(TestLlm),
            None,
        );

        let scoped = root.isolate::<ares_tools::Tools>("tenant-a");
        agent.set_tools(scoped.get::<ares_tools::Tools>().expect("Tools"));
        agent.bind_request_ctx(scoped);

        let definitions = agent.get_filtered_tool_definitions();
        assert_eq!(definitions.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["tenant_a_tool"]);
        assert!(agent.can_use_tool("tenant_a_tool"));
        assert!(!agent.can_use_tool("tenant_b_tool"));
    }
}