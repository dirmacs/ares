//! V1 chat domain — cordis Phase6
//! Bodies moved from v1.rs

use std::sync::Arc;
use ares_cordis_core::Context;
use super::*;

use crate::agents::context_provider::AgentRuntimeContext;
use crate::agents::tenant_agent;
use crate::db::agent_runs;
use crate::models::TenantContext;
use crate::research::coordinator::ResearchCoordinator;
use crate::types::{
    AgentContext, AgentType, AppError, ChatRequest, ChatResponse, ResearchRequest,
    ResearchResponse, Result,
};
use crate::AppState;
use ares_agents::Agent;
use axum::{
    extract::{Extension, State},
    response::Response,
    Json,
};

/// Resolve the tenant-isolated static tool service and inject it into the
/// legacy agent. The child context is intentionally request-local: the
/// resolved service cannot be observed by another tenant's request.
fn inject_tenant_tool_service(
    state_ctx: &Arc<Context>,
    agent: &mut ares_agents::ConfigurableAgent,
) {
    let Some(tc) = state_ctx.get::<crate::models::TenantContext>() else {
        return;
    };
    let tenant_id = tc.tenant_id.as_str();
    let tenant_ctx = state_ctx.isolate::<crate::context_services::ToolRegistryService>(tenant_id);
    let base_tools = state_ctx
        .get::<crate::context_services::ToolRegistryService>()
        .expect("ToolRegistry missing")
        .0
        .clone();
    tenant_ctx.provide(crate::context_services::ToolRegistryService(base_tools));
    let scoped_tools = tenant_ctx
        .get_isolated::<crate::context_services::ToolRegistryService>(tenant_id)
        .expect("isolated ToolRegistry missing");

    // ConfigurableAgent uses this service for definitions and dispatch, rather
    // than merely resolving it for logging. Runtime tools remain filtered by
    // the existing tenant_id path below.
    let service = ares_tools::UnifiedToolService::new(scoped_tools.0.clone());
    agent.inject_tool_service(Arc::new(service));
}

/// POST /v1/chat — tenant-scoped chat (API key auth, no conversation history)
pub async fn v1_chat(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Json(payload): Json<ChatRequest>,
) -> Result<axum::response::Response> {
    let tc = extract_tenant(ctx)?;
    // Cordis intercept: publish tenant scope so downstream ctx.get::<TenantContext>() reads it.
    let state_ctx = state_ctx.with_intercept(tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    // Per-request model override is implemented via ModelOverride + Cordis intercept
    // (see the DI path below: state_ctx.with_intercept(ModelOverride { .. })).

    // Quota enforcement — check monthly + daily request limits
    enforce_quota(&state_ctx).await?;

    // Emergency stop — kill switch for all agents
    if state_ctx.get::<crate::context_services::EmergencyStopService>().expect("not provided").0
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(crate::types::AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        ));
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
    let agent_name = crate::agents::registry::AgentRegistry::type_to_name(&agent_type).to_string();
    let start = std::time::Instant::now();

    // Inject Eruka context — the core product feature.
    // Calls the ContextProvider (ErukaContextProvider in managed mode, NoOp in OSS)
    // to fetch per-agent knowledge state and gap constraints from Eruka.
    let mut runtime_context =
        AgentRuntimeContext::new(tc.tenant_id.clone(), agent_name.clone(), "api_v1_chat");
    runtime_context.workspace_id = payload.workspace_id.clone();
    runtime_context.session_id = Some(agent_context.session_id.clone());

    let eruka_context = state_ctx.get::<crate::context_services::ContextProviderService>().expect("not provided").0
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

    use crate::agents::Agent;
    // Phase 4 §15: v1/chat delegates to AgentExecutionService for resolve+create+execute.
    // Observability (agent_runs recording) remains local since it needs v1-specific metadata
    // (workspace_id, eruka_context_hit, request_source).
    if let Some(exec_svc) = state_ctx.get::<ares_agents::execution::AgentExecutionService>() {
        let req = ares_agents::execution::AgentRequest {
            agent_name: agent_name.clone(),
            tenant: Some(tc.tenant_id.clone()),
            message: effective_message.clone(),
            history: agent_context.conversation_history.clone(),
            ctx_provider: None,
        };
        // Cordis intercepts are request-local and composable: pin the model,
        // then attach the tenant's current allowlist without mutating root state.
        let req_ctx = if let Some(m) = &payload.model {
            let pool = state_ctx
                .get::<crate::context_services::TenantDbService>()
                .expect("TenantDbService not provided")
                .0
                .pool()
                .clone();
            let allowed_models = crate::db::tenant_allowlist::TenantAllowlistStore::new(&pool)
                .list_models(&tc.tenant_id)
                .await?;
            let child = state_ctx.with_intercept(ares_agents::execution::ModelOverride {
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
        let exec_result = exec_svc.execute_agent(&req, &req_ctx).await?;
        let duration_ms = start.elapsed().as_millis() as i64;
        let response_text = exec_result.response.content;
        let (model_name, provider_name) = execution_metadata_names(exec_result.response.metadata.as_ref());
        let (input_tokens, output_tokens) =
            llm_token_counts_u32(exec_result.response.usage.as_ref(), &effective_message, &response_text);

        // Record agent run with metadata from ExecutionResult
        {
            let pool = state_ctx.get::<crate::context_services::TenantDbService>().expect("not provided").0.pool().clone();
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

    // Legacy fallback: resolve_agent_from_ctx + inline execution
    // Cordis isolate: tenant-scoped tool resolution
    let tenant_ctx = state_ctx.isolate::<crate::context_services::ToolRegistryService>(&tc.tenant_id);
    tracing::debug!(
        tenant = %tc.tenant_id,
        "v1/chat: using Cordis-isolated context for tenant tool scoping"
    );
    // Cordis intercept: per-request model override without mutating global state
    let tenant_ctx = if let Some(ref model) = payload.model {
        tracing::debug!(model = %model, "v1/chat: Cordis intercept for model override");
        tenant_ctx.intercept(ares_agents::execution::ModelOverride {
            model: model.clone(),
        })
    } else {
        tenant_ctx
    };

    let mut resolved_agent = tenant_agent::resolve_agent_from_ctx(
        state_ctx.get::<crate::context_services::TenantDbService>().expect("not provided").0.pool(),
        &state_ctx.get::<ares_agents::AgentRegistry>().expect("AgentRegistry not provided"),
        &state_ctx,
        &agent_name,
        &state_ctx.get::<crate::context_services::FleetSecretsService>().expect("not provided").0,
    )
    .await?;
    inject_tenant_tool_service(&tenant_ctx, &mut resolved_agent.agent);
    // Give the agent access to its tenant's runtime (DB-defined) tools so the
    // LLM can actually call them. Tenant-scoped — never cross-tenant.
    // cordis Phase6: runtime gating via PostgresService::check (was cfg feature postgres)
    if cfg!(feature = "postgres") {
        resolved_agent
            .agent
            .set_runtime_tools_from_ctx(tenant_ctx.get::<crate::context_services::RuntimeToolRegistryService>().expect("not provided").0.clone(), &state_ctx);
    }
    let response = resolved_agent
        .agent
        .execute(&effective_message, &agent_context)
        .await?;
    let duration_ms = start.elapsed().as_millis() as i64;

    let response_text = response.content;
    let (model_name, provider_name) = execution_metadata_names(response.metadata.as_ref());

    // Use actual LLM token counts; fall back to heuristic estimates if unavailable
    let (input_tokens, output_tokens) =
        llm_token_counts_u32(response.usage.as_ref(), &effective_message, &response_text);

    // Record agent run with real model/provider
    {
        let pool = state_ctx.get::<crate::context_services::TenantDbService>().expect("not provided").0.pool().clone();
        let tid = tc.tenant_id.clone();
        let aname = resolved_agent.agent_name.clone();
        let itok = input_tokens as i64;
        let otok = output_tokens as i64;
        let mname = model_name.clone();
        let pname = provider_name.clone();
        let metadata = agent_runs::AgentRunMetadata {
            workspace_id: payload.workspace_id.clone(),
            session_id: Some(agent_context.session_id.clone()),
            request_source: Some("api_v1_chat".to_string()),
            product: None,
            agent_config_source: Some(resolved_agent.source.as_str().to_string()),
            agent_config_version: resolved_agent.config_version.clone(),
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
                &pool,
                &tid,
                &aname,
                None,
                "completed",
                itok,
                otok,
                duration_ms,
                None,
                &mname,
                &pname,
                false,
                Some(&metadata),
            )
            .await;
        });
    }

    let chat_response = ChatResponse {
        response: response_text,
        agent: format!(
            "{} ({})",
            resolved_agent.agent_name,
            resolved_agent.source.as_str()
        ),
        context_id: agent_context.session_id,
        sources: None,
    };

    let mut response = usage_response(
        chat_response,
        input_tokens as u64,
        output_tokens as u64,
        &model_name,
        &provider_name,
        &resolved_agent.agent_name,
    );
    set_header(
        response.headers_mut(),
        "x-agent-config-source",
        resolved_agent.source.as_str(),
    );
    if let Some(config_version) = &resolved_agent.config_version {
        set_header(
            response.headers_mut(),
            "x-agent-config-version",
            config_version,
        );
    }
    if let Some(workspace_id) = &payload.workspace_id {
        set_header(
            response.headers_mut(),
            "x-runtime-workspace-id",
            workspace_id,
        );
    }
    Ok(response)
}

async fn ensure_research_model_allowed(
    state_ctx: &AppState,
    tenant_id: &str,
    model_name: &str,
) -> Result<()> {
    let pool = state_ctx.get::<crate::context_services::TenantDbService>().expect("not provided").0.pool().clone();
    let allowlist_store = crate::db::tenant_allowlist::TenantAllowlistStore::new(&pool);
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
    Err(AppError::Auth(format!(
        "Model '{}' is not allowed for this tenant",
        model_name
    )))
}

/// POST /v1/research — tenant-scoped research with provider-reported metering.
pub async fn v1_research(
    State(state_ctx): State<Arc<Context>>,
    ctx: Option<Extension<TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Json(payload): Json<ResearchRequest>,
) -> Result<Response> {
    let tc = extract_tenant(ctx)?;
    // Cordis intercept: publish tenant scope so downstream ctx.get::<TenantContext>() reads it.
    let state_ctx = state_ctx.with_intercept(tc.clone());
    let state_ctx = match usage {
        Some(Extension(u)) => state_ctx.with_intercept(u),
        None => state_ctx,
    };
    enforce_quota(&state_ctx).await?;

    if state_ctx.get::<crate::context_services::EmergencyStopService>().expect("not provided").0
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        ));
    }

    let start = std::time::Instant::now();
    let config = state_ctx.get::<crate::context_services::ConfigManagerService>().expect("not provided").0.config();
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

    let llm_client = match state_ctx.get::<crate::context_services::ProviderRegistryService>().expect("not provided").0
        .create_client_for_model_ctx(&state_ctx, model_key)
        .await
    {
        Ok(client) => client,
        Err(_) => state_ctx.get::<ares_llm::provider_registry::ConfigBasedLLMFactory>().expect("LlmFactory not provided").create_default().await?,
    };
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

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::post;
    axum::Router::new()
        .route("/v1/chat/v1_chat", post(v1_chat))
        .route("/v1/chat/v1_research", post(v1_research))
}

// cordis Phase6: RouteSet Service
use ares_cordis_core::Service;
pub struct V1ChatService;
impl Service for V1ChatService {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TenantTier;
    use ares_agents::ConfigurableAgent;
    use ares_config::toml_config::AgentConfig;
    use ares_llm::{LLMClient, LLMResponse};
    use ares_tools::registry::{Tool, ToolRegistry};
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

        async fn execute(&self, _args: Value) -> crate::types::Result<Value> {
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
        async fn generate(&self, _prompt: &str) -> crate::types::Result<String> {
            Ok(String::new())
        }

        async fn generate_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> crate::types::Result<String> {
            Ok(String::new())
        }

        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> crate::types::Result<LLMResponse> {
            Ok(test_response())
        }

        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ToolDefinition],
        ) -> crate::types::Result<LLMResponse> {
            Ok(test_response())
        }

        async fn generate_with_tools_and_history(
            &self,
            _messages: &[ares_llm::coordinator::ConversationMessage],
            _tools: &[ToolDefinition],
        ) -> crate::types::Result<LLMResponse> {
            Ok(test_response())
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> crate::types::Result<Box<dyn futures::Stream<Item = crate::types::Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> crate::types::Result<Box<dyn futures::Stream<Item = crate::types::Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> crate::types::Result<Box<dyn futures::Stream<Item = crate::types::Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }

        fn model_name(&self) -> &str {
            "test"
        }
    }

    #[test]
    fn tenant_isolated_service_reaches_agent_and_denies_other_tenant_tool() {
        let root = Context::new_root();
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TestTool("tenant_a_tool")));
        registry.register(Arc::new(TestTool("tenant_b_tool")));
        root.provide(crate::context_services::ToolRegistryService(Arc::new(registry)));

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

        let scoped = root.with_intercept(TenantContext::new("tenant-a".into(), TenantTier::Free));
        inject_tenant_tool_service(&scoped, &mut agent);

        let definitions = agent.get_filtered_tool_definitions();
        assert_eq!(definitions.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["tenant_a_tool"]);
        assert!(agent.can_use_tool("tenant_a_tool"));
        assert!(!agent.can_use_tool("tenant_b_tool"));
    }
}