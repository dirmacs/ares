//! V1 chat domain — cordis Phase6
//! Bodies moved from v1.rs

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

/// POST /v1/chat — tenant-scoped chat (API key auth, no conversation history)
pub async fn v1_chat(
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Json(payload): Json<ChatRequest>,
) -> Result<axum::response::Response> {
    let tc = extract_tenant(ctx)?;

    // Quota enforcement — check monthly + daily request limits
    enforce_quota(&state, &tc).await?;

    // Emergency stop — kill switch for all agents
    if state
        .emergency_stop
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

    let eruka_context = state
        .context_provider
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
    let mut resolved_agent = tenant_agent::resolve_agent_for_tenant(
        state.tenant_db.pool(),
        &state.agent_registry,
        &tc.tenant_id,
        &agent_name,
        &state.fleet_secrets,
    )
    .await?;
    // Give the agent access to its tenant's runtime (DB-defined) tools so the
    // LLM can actually call them. Tenant-scoped — never cross-tenant.
    #[cfg(feature = "postgres")]
    resolved_agent
        .agent
        .set_runtime_tools(state.runtime_tool_registry.clone(), tc.tenant_id.clone());
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
        let pool = state.tenant_db.pool().clone();
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
    state: &AppState,
    tenant_id: &str,
    model_name: &str,
) -> Result<()> {
    let allowlist_store =
        crate::db::tenant_allowlist::TenantAllowlistStore::new(state.tenant_db.pool());
    research_model_allowlist_decision(
        allowlist_store
            .is_model_allowed(tenant_id, model_name)
            .await?,
        model_name,
    )
}

fn research_model_allowlist_decision(is_allowed: bool, model_name: &str) -> Result<()> {
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
    State(state): State<AppState>,
    ctx: Option<Extension<TenantContext>>,
    Json(payload): Json<ResearchRequest>,
) -> Result<Response> {
    let tc = extract_tenant(ctx)?;
    enforce_quota(&state, &tc).await?;

    if state
        .emergency_stop
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        ));
    }

    let start = std::time::Instant::now();
    let config = state.config_manager.config();
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

    let llm_client = match state
        .provider_registry
        .create_client_for_model(model_key)
        .await
    {
        Ok(client) => client,
        Err(_) => state.llm_factory.create_default().await?,
    };
    let model_name = llm_client.model_name().to_string();
    ensure_research_model_allowed(&state, &tc.tenant_id, &model_name).await?;

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

// Service stub for v1_chat