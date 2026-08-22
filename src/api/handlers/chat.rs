use crate::db::postgres::UserAgent;
use crate::{
    agents::{registry::AgentRegistry, router::RouterAgent, Agent},
    api::handlers::user_agents::resolve_agent,
    auth::middleware::AuthUser,
    db::agent_runs,
    memory::estimate_tokens,
    observability::RunObservability,
    types::{
        AgentContext, AgentType, AppError, ChatRequest, ChatResponse, Claims, MessageRole, Result,
        UserMemory,
    },
    utils::toml_config::AgentConfig,
    AppState,
};
use axum::{
    extract::{Query, State},
    response::Response,
    Extension, Json,
};
use std::sync::Arc;
use ares_cordis_core::Context;
use uuid::Uuid;

/// Validates chat request payload before routing.
fn validate_chat_request(payload: &ChatRequest) -> Result<()> {
    if payload.message.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "message must not be empty".to_string(),
        ));
    }
    Ok(())
}

/// Returns the provided context id or generates a new UUID string.
fn resolve_context_id(context_id: Option<&String>) -> String {
    context_id
        .cloned()
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

/// Rejects direct invocation of the router agent type.
fn ensure_not_direct_router(agent_type: &AgentType) -> Result<()> {
    if *agent_type == AgentType::Router {
        return Err(AppError::InvalidInput(
            "Router agent cannot be called directly".to_string(),
        ));
    }
    Ok(())
}

/// Builds the LLM prompt used for streaming chat.
fn build_stream_prompt(system_prompt: &str, message: &str) -> String {
    format!(
        "{system_prompt}\n\nUser: {message}\nAssistant:",
        system_prompt = system_prompt,
        message = message,
    )
}

fn default_stream_system_prompt() -> &'static str {
    "You are a helpful assistant."
}

pub(crate) fn emergency_stop_message() -> &'static str {
    "All agents are currently under human review. Please try again later."
}

fn ensure_emergency_stop_inactive(stop: &crate::context_services::EmergencyStop) -> Result<()> {
    if stop.is_active() {
        return Err(AppError::Unavailable(emergency_stop_message().to_string()));
    }
    Ok(())
}

/// Converts a resolved user agent record into runtime agent configuration.
fn agent_config_from_user_agent(user_agent: &UserAgent) -> AgentConfig {
    AgentConfig {
        model: user_agent.model.clone(),
        system_prompt: user_agent.system_prompt.clone(),
        tools: user_agent.tools_vec(),
        max_tool_iterations: user_agent.max_tool_iterations as usize,
        parallel_tools: user_agent.parallel_tools,
        allowed_tools: None,
        extra: std::collections::HashMap::new(),
    }
}

/// Returns user memory when facts or preferences are present.
pub(crate) fn build_user_memory_if_present(
    user_id: &str,
    facts: Vec<crate::types::MemoryFact>,
    preferences: Vec<crate::types::Preference>,
) -> Option<UserMemory> {
    if facts.is_empty() && preferences.is_empty() {
        None
    } else {
        Some(UserMemory {
            user_id: user_id.to_string(),
            preferences,
            facts,
        })
    }
}

/// Picks the streaming system prompt from agent config or the default.
pub(crate) fn resolve_stream_system_prompt(user_agent: &UserAgent) -> String {
    user_agent
        .system_prompt
        .clone()
        .unwrap_or_else(|| default_stream_system_prompt().to_string())
}

/// Resolves token counts from LLM usage or heuristic estimates.
pub(crate) fn resolve_token_counts(
    usage: Option<&crate::llm::client::TokenUsage>,
    history_input_tokens: usize,
    message: &str,
    response: &str,
) -> (u32, u32) {
    if let Some(u) = usage {
        (u.prompt_tokens, u.completion_tokens)
    } else {
        (
            (history_input_tokens + estimate_tokens(message)) as u32,
            estimate_tokens(response) as u32,
        )
    }
}

/// Builds a chat response payload from agent execution output.
pub(crate) fn chat_response_from_agent_output(
    agent_type: AgentType,
    source: &str,
    context_id: &str,
    content: String,
) -> ChatResponse {
    ChatResponse {
        response: content,
        agent: format!("{:?} ({})", agent_type, source),
        context_id: context_id.to_string(),
        sources: None,
    }
}

pub(crate) fn stream_error_event(error: &str, context_id: Option<&str>) -> StreamEvent {
    StreamEvent {
        event: "error".to_string(),
        content: None,
        agent: None,
        context_id: context_id.map(str::to_string),
        error: Some(error.to_string()),
    }
}

pub(crate) fn stream_start_event(agent_type: &AgentType, context_id: &str) -> StreamEvent {
    StreamEvent {
        event: "start".to_string(),
        content: None,
        agent: Some(format!("{} (system)", agent_type)),
        context_id: Some(context_id.to_string()),
        error: None,
    }
}

pub(crate) fn stream_token_event(token: &str) -> StreamEvent {
    StreamEvent {
        event: "token".to_string(),
        content: Some(token.to_string()),
        agent: None,
        context_id: None,
        error: None,
    }
}

pub(crate) fn stream_done_event(
    agent_type: &AgentType,
    source: &str,
    context_id: &str,
) -> StreamEvent {
    StreamEvent {
        event: "done".to_string(),
        content: None,
        agent: Some(format!("{:?} ({})", agent_type, source)),
        context_id: Some(context_id.to_string()),
        error: None,
    }
}

/// Chat with the AI assistant
#[utoipa::path(
    post,
    path = "/api/chat",
    request_body = ChatRequest,
    responses(
        (status = 200, description = "Chat response", body = ChatResponse),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized")
    ),
    tag = "chat",
    security(("bearer" = []))
)]
pub async fn chat(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    tenant_ctx: Option<Extension<crate::models::TenantContext>>,
    usage: Option<Extension<crate::middleware::usage::UsageContext>>,
    Json(payload): Json<ChatRequest>,
) -> Result<Response> {
    // Cordis intercept: publish tenant scope so downstream ctx.get::<TenantContext>() reads it.
    let ctx = match &tenant_ctx {
        Some(Extension(tc)) => ctx.with_intercept(tc.clone()),
        None => ctx,
    };
    let ctx = match usage {
        Some(Extension(u)) => ctx.with_intercept(u),
        None => ctx,
    };
    validate_chat_request(&payload)?;
    ensure_emergency_stop_inactive(&ctx.get::<crate::context_services::EmergencyStop>().expect("not provided"))?;

    // Get or create conversation
    let context_id = resolve_context_id(payload.context_id.as_ref());

    // Check if conversation exists, create if not
    if !ctx.get::<crate::db::PostgresClient>().expect("not provided").conversation_exists(&context_id).await? {
        ctx.get::<crate::db::PostgresClient>().expect("not provided")
            .create_conversation(&context_id, &claims.sub, None)
            .await?;
    }
    let history = ctx.get::<crate::db::PostgresClient>().expect("not provided").get_conversation_history(&context_id).await?;
    // Compute history token estimate in the same pass (before clone into AgentContext)
    let history_input_tokens: usize = history.iter().map(|m| estimate_tokens(&m.content)).sum();

    // Load user memory
    let memory_facts = ctx.get::<crate::db::PostgresClient>().expect("not provided").get_user_memory(&claims.sub).await?;
    let preferences = ctx.get::<crate::db::PostgresClient>().expect("not provided").get_user_preferences(&claims.sub).await?;
    let user_memory = build_user_memory_if_present(&claims.sub, memory_facts, preferences);

    // Build agent context
    let agent_context = AgentContext {
        user_id: claims.sub.clone(),
        session_id: context_id.clone(),
        conversation_history: history.clone(),
        user_memory,
    };

    // Route to appropriate agent
    let agent_type = if let Some(at) = payload.agent_type {
        at
    } else {
        // Get router model from config, or use default
        let config = ctx.get::<crate::AresConfigManager>().expect("not provided").config();
        let router_model = config
            .get_agent("router")
            .map(|a| a.model.as_str())
            .unwrap_or("fast");

        let router_llm = match ctx.get::<crate::ProviderRegistry>().expect("not provided")
            .create_client_for_model_ctx(&ctx, router_model)
            .await
        {
            Ok(client) => client,
            Err(_) => ctx.get::<ares_llm::provider_registry::ConfigBasedLLMFactory>().expect("LlmFactory not provided").create_default().await?,
        };

        let router = RouterAgent::new(router_llm);
        router.route(&payload.message, &agent_context).await?
    };

    // Execute agent with timing
    let agent_name_for_run = AgentRegistry::type_to_name(&agent_type).to_string();
    let start = std::time::Instant::now();
    let (response, usage) =
        execute_agent(agent_type, &payload.message, &agent_context, &ctx).await?;
    let duration_ms = start.elapsed().as_millis() as i64;

    // Store messages in conversation
    let msg_id = Uuid::new_v4().to_string();
    ctx.get::<crate::db::PostgresClient>().expect("not provided")
        .add_message(&msg_id, &context_id, MessageRole::User, &payload.message)
        .await?;

    let resp_id = Uuid::new_v4().to_string();
    ctx.get::<crate::db::PostgresClient>().expect("not provided")
        .add_message(
            &resp_id,
            &context_id,
            MessageRole::Assistant,
            &response.response,
        )
        .await?;

    // Use actual LLM token counts; fall back to heuristic estimates if unavailable
    let (input_tokens, output_tokens) = resolve_token_counts(
        usage.as_ref(),
        history_input_tokens,
        &payload.message,
        &response.response,
    );

    // Record agent run (fire-and-forget)
    {
        let pool = ctx.get::<crate::TenantDb>().expect("not provided").pool().clone();
        let agent_name = agent_name_for_run;
        let user_id = claims.sub.clone();
        let tenant_id_for_run = tenant_ctx
            .map(|Extension(tc)| tc.tenant_id.clone())
            .unwrap_or_else(|| "system".to_string());
        let itok = input_tokens as i64;
        let otok = output_tokens as i64;
        let metadata = agent_runs::AgentRunMetadata {
            workspace_id: payload.workspace_id.clone(),
            session_id: Some(context_id.clone()),
            request_source: Some("api_chat".to_string()),
            product: None,
            agent_config_source: None,
            agent_config_version: None,
            eruka_binding_id: None,
            ..Default::default()
        };
        tokio::spawn(async move {
            let _ = agent_runs::insert_agent_run_with_metadata(
                &pool,
                &tenant_id_for_run,
                &agent_name,
                Some(&user_id),
                "completed",
                itok,
                otok,
                duration_ms,
                None,
                "unknown",
                "unknown",
                false,
                Some(&metadata),
            )
            .await;
        });
    }

    let body = Json(response);
    let mut response = body.into_response();
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("x-input-tokens"),
        axum::http::HeaderValue::from(input_tokens),
    );
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("x-output-tokens"),
        axum::http::HeaderValue::from(output_tokens),
    );

    Ok(response)
}

async fn execute_agent(
    agent_type: AgentType,
    message: &str,
    context: &AgentContext,
    ctx: &AppState,
) -> Result<(ChatResponse, Option<crate::llm::client::TokenUsage>)> {
    // Get agent name from type
    let agent_name = AgentRegistry::type_to_name(&agent_type);

    ensure_not_direct_router(&agent_type)?;

    // Cordis DI path: delegate core execution to AgentExecutionService (Phase 4 §15)
    if let Some(exec_svc) = ctx.get::<ares_agents::execution::AgentExecutionService>() {
        let req = ares_agents::execution::AgentRequest {
            agent_name: agent_name.to_string(),
            message: message.to_string(),
            history: context.conversation_history.clone(),
            ctx_provider: None,
        };
        let exec_ctx = if ctx.get::<crate::models::TenantContext>().is_none() {
            ctx.isolate::<ares_agents::resolver::AgentResolverService>(format!(
                "user:{}",
                context.user_id
            ))
        } else {
            ctx.clone()
        };
        let exec_result = exec_svc.execute_agent(&req, &exec_ctx).await?;
        return Ok((
            chat_response_from_agent_output(agent_type, exec_result.source.as_str(), &context.session_id, exec_result.response.content),
            exec_result.response.usage.map(|u| crate::llm::client::TokenUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        ));
    }

    // Resolve agent using the 3-tier hierarchy (User -> Community -> System)
    let (user_agent, source) =
        resolve_agent(ctx, &context.user_id, agent_name.to_string()).await?;

    let config = agent_config_from_user_agent(&user_agent);

    // Create agent from registry using the resolved config
    let mut agent = ctx.get::<ares_agents::AgentRegistry>().expect("AgentRegistry not provided")
        .create_agent_from_config_with_fallbacks(
            agent_name,
            &config,
            &context.user_id, &ctx.get::<crate::TenantDb>().expect("not provided").pool().clone(),
            &ctx.get::<crate::FleetSecrets>().expect("not provided"),
        )
        .await?;

    // Attach observability
    let run_id = uuid::Uuid::new_v4().to_string();
    let obs = Arc::new(RunObservability {
        run_id: run_id.clone(),
        tenant_id: context.user_id.clone(),
        agent_name: agent_name.to_string(),
        pool: ctx.get::<crate::TenantDb>().expect("not provided").pool().clone(),
    });
    agent.set_observability(obs.clone());
    agent.set_run_id(run_id.clone());

    ctx.get::<crate::active_runs::ActiveRuns>().expect("not provided").start(crate::active_runs::ActiveRun {
        run_id: run_id.clone(),
        tenant_id: context.user_id.clone(),
        agent_name: agent_name.to_string(),
        started_at: chrono::Utc::now().timestamp(),
        status: "running".to_string(),
        current_step: 0,
        total_steps: 0,
        last_update: chrono::Utc::now().timestamp(),
        tool_name: None,
        model: None,
        is_catchup: false,
        request_source: Some("api_chat".to_string()),
        pipeline_id: None,
        schedule_id: None,
        trigger_id: None,
    });

    // Execute the agent
    let start = std::time::Instant::now();
    let agent_resp = match agent.execute(message, context).await {
        Ok(resp) => {
            ctx.get::<crate::active_runs::ActiveRuns>().expect("not provided").finish(&run_id, "completed");
            resp
        }
        Err(e) => {
            ctx.get::<crate::active_runs::ActiveRuns>().expect("not provided").finish(&run_id, "error");
            return Err(e);
        }
    };
    let duration_ms = start.elapsed().as_millis() as i64;

    // Aggregate run costs (fire-and-forget)
    tokio::spawn(async move {
        obs.aggregate_run_cost(duration_ms).await;
    });

    Ok((
        chat_response_from_agent_output(
            agent_type,
            &source,
            &context.session_id,
            agent_resp.content,
        ),
        agent_resp.usage,
    ))
}

/// Get user memory
#[utoipa::path(
    get,
    path = "/api/memory",
    responses(
        (status = 200, description = "User memory retrieved successfully"),
        (status = 401, description = "Unauthorized")
    ),
    tag = "chat",
    security(("bearer" = []))
)]
pub async fn get_user_memory(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
) -> Result<Json<UserMemory>> {
    let facts = ctx.get::<crate::db::PostgresClient>().expect("not provided").get_user_memory(&claims.sub).await?;
    let preferences = ctx.get::<crate::db::PostgresClient>().expect("not provided").get_user_preferences(&claims.sub).await?;

    Ok(Json(UserMemory {
        user_id: claims.sub,
        preferences,
        facts,
    }))
}

/// Streaming chat response event
#[derive(serde::Serialize)]
pub struct StreamEvent {
    /// Event type: "start", "token", "done", "error"
    pub event: String,
    /// Token content (for "token" events)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Agent type that handled the request (for "start" and "done" events)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Context ID for the conversation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// Error message (for "error" events)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ChatStreamQuery {
    pub message: String,
    #[serde(default)]
    pub agent_type: Option<AgentType>,
    #[serde(default)]
    pub context_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
}

impl From<ChatStreamQuery> for ChatRequest {
    fn from(query: ChatStreamQuery) -> Self {
        Self {
            message: query.message,
            agent_type: query.agent_type,
            context_id: query.context_id,
            workspace_id: query.workspace_id,
            model: None,
        }
    }
}

/// Stream a chat response using Server-Sent Events
#[utoipa::path(
    post,
    path = "/api/chat/stream",
    request_body = ChatRequest,
    responses(
        (status = 200, description = "Streaming chat response"),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized")
    ),
    tag = "chat",
    security(("bearer" = []))
)]
pub async fn chat_stream(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Json(payload): Json<ChatRequest>,
) -> axum::response::Sse<
    impl futures::Stream<
        Item = std::result::Result<axum::response::sse::Event, std::convert::Infallible>,
    >,
> {
    chat_stream_response(ctx, claims, payload)
}

/// Stream a chat response using EventSource-compatible query parameters.
#[utoipa::path(
    get,
    path = "/api/chat/stream",
    params(
        ("message" = String, Query, description = "Message to send to the agent"),
        ("agent_type" = Option<AgentType>, Query, description = "Optional agent type"),
        ("context_id" = Option<String>, Query, description = "Optional conversation context ID"),
        ("workspace_id" = Option<String>, Query, description = "Optional Eruka workspace ID")
    ),
    responses(
        (status = 200, description = "Streaming chat response"),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized")
    ),
    tag = "chat",
    security(("bearer" = []))
)]
pub async fn chat_stream_get(
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Query(query): Query<ChatStreamQuery>,
) -> axum::response::Sse<
    impl futures::Stream<
        Item = std::result::Result<axum::response::sse::Event, std::convert::Infallible>,
    >,
> {
    chat_stream_response(ctx, claims, query.into())
}

fn chat_stream_response(
    ctx: AppState,
    claims: Claims,
    payload: ChatRequest,
) -> axum::response::Sse<
    impl futures::Stream<
        Item = std::result::Result<axum::response::sse::Event, std::convert::Infallible>,
    >,
> {
    use axum::response::sse::{Event, Sse};

    let validation_error = validate_chat_request(&payload)
        .and_then(|_| ensure_emergency_stop_inactive(&ctx.get::<crate::context_services::EmergencyStop>().expect("not provided")))
        .err();

    // Get or create conversation
    let context_id = resolve_context_id(payload.context_id.as_ref());

    // Clone values we need for the async stream
    let state_clone = ctx.clone();
    let claims_clone = claims.clone();
    let message = payload.message.clone();
    let agent_type_req = payload.agent_type;
    let runtime_workspace_id = payload.workspace_id.clone();
    let context_id_clone = context_id.clone();
    let active_runs = Arc::clone(&ctx.get::<crate::active_runs::ActiveRuns>().expect("not provided"));

    let stream = async_stream::stream! {
        // Cordis: hold Context-derived services for stream (avoid temp dropped)
        let db = state_clone.get::<crate::db::PostgresClient>().expect("not provided");
        let config_manager = state_clone.get::<crate::AresConfigManager>().expect("not provided").clone();
        let provider_registry = state_clone.get::<crate::ProviderRegistry>().expect("not provided").clone();
        let llm_factory = state_clone.get::<ares_llm::provider_registry::ConfigBasedLLMFactory>().expect("LlmFactory not provided").clone();
        let tenant_db = state_clone.get::<crate::TenantDb>().expect("not provided").clone();
        if let Some(e) = &validation_error {
            let event = stream_error_event(&e.to_string(), None);
            yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
            return;
        }

        // Setup conversation
        if !db.conversation_exists(&context_id_clone).await.unwrap_or(false) {
            if let Err(e) = db
                .create_conversation(&context_id_clone, &claims_clone.sub, None)
                .await {
                tracing::warn!("Failed to create conversation {}: {}", context_id_clone, e);
            }
        }

        let history = db.get_conversation_history(&context_id_clone).await.unwrap_or_else(|e| {
            tracing::warn!("Failed to get conversation history for {}: {}", context_id_clone, e);
            vec![]
        });

        // Load user memory
        let memory_facts = db.get_user_memory(&claims_clone.sub).await.unwrap_or_else(|e| {
            tracing::warn!("Failed to get user memory for {}: {}", claims_clone.sub, e);
            vec![]
        });
        let preferences = db.get_user_preferences(&claims_clone.sub).await.unwrap_or_else(|e| {
            tracing::warn!("Failed to get user preferences for {}: {}", claims_clone.sub, e);
            vec![]
        });
        let user_memory = build_user_memory_if_present(
            &claims_clone.sub,
            memory_facts,
            preferences,
        );

        // Build agent context
        let agent_context = AgentContext {
            user_id: claims_clone.sub.clone(),
            session_id: context_id_clone.clone(),
            conversation_history: history,
            user_memory,
        };

        // Route to appropriate agent
        let agent_type = if let Some(at) = agent_type_req {
            at
        } else {
            let config = config_manager.config();
            let router_model = config
                .get_agent("router")
                .map(|a| a.model.as_str())
                .unwrap_or("fast");

            let router_llm = match provider_registry
                .create_client_for_model_ctx(&state_clone, router_model)
                .await
            {
                Ok(client) => client,
                Err(_) => match llm_factory.create_default().await {
                    Ok(c) => c,
                    Err(e) => {
                        let event = stream_error_event(
                            &format!("Failed to create LLM client: {}", e),
                            Some(&context_id_clone),
                        );
                        yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                        return;
                    }
                },
            };

            let router = RouterAgent::new(router_llm);
            match router.route(&message, &agent_context).await {
                Ok(t) => t,
                Err(e) => {
                    let event = stream_error_event(
                        &format!("Router failed: {}", e),
                        Some(&context_id_clone),
                    );
                    yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                    return;
                }
            }
        };

        // Send start event
        let agent_name = AgentRegistry::type_to_name(&agent_type);
        let stream_run_id = uuid::Uuid::new_v4().to_string();
        active_runs.start(crate::active_runs::ActiveRun {
            run_id: stream_run_id.clone(),
            tenant_id: claims_clone.sub.clone(),
            agent_name: agent_name.to_string(),
            started_at: chrono::Utc::now().timestamp(),
            status: "running".to_string(),
            current_step: 0,
            total_steps: 0,
            last_update: chrono::Utc::now().timestamp(),
            tool_name: None,
            model: None,
            is_catchup: false,
            request_source: Some("api_chat_stream".to_string()),
            pipeline_id: None,
            schedule_id: None,
            trigger_id: None,
        });
        let start_event = stream_start_event(&agent_type, &context_id_clone);
        yield Ok(Event::default().data(serde_json::to_string(&start_event).unwrap_or_default()));

        // Resolve agent using hierarchy
        let (user_agent, source) = match crate::api::handlers::user_agents::resolve_agent(
            &state_clone,
            &claims_clone.sub,
            agent_name.to_string(),
        ).await {
            Ok(r) => r,
            Err(e) => {
                let event = stream_error_event(
                    &format!("Failed to resolve agent: {}", e),
                    Some(&context_id_clone),
                );
                yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                return;
            }
        };

        // Get LLM client for streaming
        let llm = match provider_registry
            .create_client_for_model_ctx(&state_clone, &user_agent.model)
            .await
        {
            Ok(c) => c,
            Err(_) => match llm_factory.create_default().await {
                Ok(c) => c,
                Err(e) => {
                    let event = stream_error_event(
                        &format!("Failed to create LLM: {}", e),
                        Some(&context_id_clone),
                    );
                    yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                    return;
                }
            },
        };

        // Build the prompt with system message and history
        let system_prompt = resolve_stream_system_prompt(&user_agent);
        let full_prompt = build_stream_prompt(&system_prompt, &message);
        active_runs.update_model(&stream_run_id, Some(&user_agent.model));
        active_runs.update(&stream_run_id, "llm_call", 1);

        // Stream tokens
        use futures::StreamExt;
        let mut full_response = String::new();
        match llm.stream(&full_prompt).await {
            Ok(mut token_stream) => {
                while let Some(token_result) = token_stream.next().await {
                    match token_result {
                        Ok(token) => {
                            full_response.push_str(&token);
                            let event = stream_token_event(&token);
                            yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                        }
                        Err(e) => {
                            active_runs.finish(&stream_run_id, "error");
                            let event = stream_error_event(
                                &format!("Stream error: {}", e),
                                Some(&context_id_clone),
                            );
                            yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                active_runs.finish(&stream_run_id, "error");
                let event = stream_error_event(
                    &format!("Failed to start stream: {}", e),
                    Some(&context_id_clone),
                );
                yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                return;
            }
        }

        // Store messages in conversation
        let msg_id = Uuid::new_v4().to_string();
        if let Err(e) = db
            .add_message(&msg_id, &context_id_clone, MessageRole::User, &message)
            .await {
            tracing::error!("Failed to store user message in conversation {}: {}", context_id_clone, e);
        }

        let resp_id = Uuid::new_v4().to_string();
        if let Err(e) = db
            .add_message(&resp_id, &context_id_clone, MessageRole::Assistant, &full_response)
            .await {
            tracing::error!("Failed to store assistant message in conversation {}: {}", context_id_clone, e);
        }

        // Record agent run for billing (streaming calls were previously invisible)
        {
            let pool = tenant_db.pool().clone();
            let tid = claims_clone.sub.clone();
            let aname = agent_name.to_string();
            let itok = crate::memory::estimate_tokens(&message) as i64;
            let otok = crate::memory::estimate_tokens(&full_response) as i64;
            let model = user_agent.model.clone();
            let metadata = crate::db::agent_runs::AgentRunMetadata {
                workspace_id: runtime_workspace_id.clone(),
                session_id: Some(context_id_clone.clone()),
                request_source: Some("api_chat_stream".to_string()),
                product: None,
                agent_config_source: Some(source.to_string()),
                agent_config_version: None,
                eruka_binding_id: None,
                ..Default::default()
            };
            tokio::spawn(async move {
                let _ = crate::db::agent_runs::insert_agent_run_with_metadata(
                    &pool, &tid, &aname, Some(&tid), "completed",
                    itok, otok, 0, None, &model, "unknown", true, Some(&metadata),
                )
                .await;
            });
        }

        active_runs.finish(&stream_run_id, "completed");

        // Send done event
        let done_event = stream_done_event(&agent_type, &source, &context_id_clone);
        yield Ok(Event::default().data(serde_json::to_string(&done_event).unwrap_or_default()));
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    )
}
use axum::response::IntoResponse;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MemoryFact, Message, MessageRole, Preference, Source};
    use chrono::Utc;

    fn sample_user_agent(tools_json: &str) -> UserAgent {
        UserAgent {
            id: "ua-1".into(),
            user_id: "user-1".into(),
            name: "finance".into(),
            display_name: None,
            description: None,
            model: "fast".into(),
            system_prompt: Some("You are finance.".into()),
            tools: tools_json.into(),
            max_tool_iterations: 5,
            parallel_tools: true,
            extra: "{}".into(),
            is_public: false,
            usage_count: 0,
            rating_sum: 0,
            rating_count: 0,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_100,
        }
    }

    fn sample_preference() -> Preference {
        Preference {
            category: "communication".into(),
            key: "tone".into(),
            value: "formal".into(),
            confidence: 0.8,
        }
    }

    fn sample_fact() -> MemoryFact {
        MemoryFact {
            id: "fact-1".into(),
            user_id: "user-1".into(),
            category: "work".into(),
            fact_key: "role".into(),
            fact_value: "engineer".into(),
            confidence: 0.9,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn validate_chat_request_rejects_whitespace_only_message() {
        let payload = ChatRequest {
            message: "   ".into(),
            agent_type: None,
            context_id: None,
            workspace_id: None,
            model: None,
        };
        let err = validate_chat_request(&payload).unwrap_err();
        match err {
            AppError::InvalidInput(msg) => assert!(msg.contains("empty")),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn validate_chat_request_rejects_empty_string_message() {
        let payload = ChatRequest {
            message: String::new(),
            agent_type: None,
            context_id: None,
            workspace_id: None,
            model: None,
        };
        assert!(validate_chat_request(&payload).is_err());
    }

    #[test]
    fn validate_chat_request_accepts_non_empty_message() {
        let payload = ChatRequest {
            message: "hello".into(),
            agent_type: None,
            context_id: None,
            workspace_id: None,
            model: None,
        };
        assert!(validate_chat_request(&payload).is_ok());
    }

    #[test]
    fn validate_chat_request_accepts_message_with_surrounding_whitespace() {
        let payload = ChatRequest {
            message: "  hello  ".into(),
            agent_type: None,
            context_id: None,
            workspace_id: None,
            model: None,
        };
        assert!(validate_chat_request(&payload).is_ok());
    }

    #[test]
    fn resolve_context_id_uses_existing_value() {
        let existing = "ctx-42".to_string();
        assert_eq!(resolve_context_id(Some(&existing)), "ctx-42");
    }

    #[test]
    fn resolve_context_id_generates_uuid_when_missing() {
        let id = resolve_context_id(None);
        assert!(Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn resolve_context_id_generates_canonical_uuid_format() {
        let id = resolve_context_id(None);
        let parsed = Uuid::parse_str(&id).expect("valid uuid");
        assert_eq!(parsed.get_version(), Some(uuid::Version::Random));
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[8], b'-');
        assert_eq!(id.as_bytes()[13], b'-');
    }

    #[test]
    fn ensure_not_direct_router_rejects_router_type() {
        let err = ensure_not_direct_router(&AgentType::Router).unwrap_err();
        match err {
            AppError::InvalidInput(msg) => assert!(msg.contains("Router")),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn ensure_not_direct_router_allows_product_agent() {
        assert!(ensure_not_direct_router(&AgentType::Product).is_ok());
    }

    #[test]
    fn ensure_not_direct_router_allows_orchestrator_agent() {
        assert!(ensure_not_direct_router(&AgentType::Orchestrator).is_ok());
    }

    #[test]
    fn ensure_not_direct_router_allows_invoice_sales_finance_hr_agents() {
        for agent in [
            AgentType::Invoice,
            AgentType::Sales,
            AgentType::Finance,
            AgentType::HR,
        ] {
            assert!(ensure_not_direct_router(&agent).is_ok(), "{agent:?}");
        }
    }

    #[test]
    fn ensure_not_direct_router_allows_custom_agent() {
        assert!(ensure_not_direct_router(&AgentType::Custom("my-bot".into())).is_ok());
    }

    #[test]
    fn ensure_emergency_stop_inactive_rejects_active_stop() {
        let active = crate::context_services::EmergencyStop::new(true);
        let err = ensure_emergency_stop_inactive(&active).unwrap_err();
        match err {
            AppError::Unavailable(msg) => assert_eq!(msg, emergency_stop_message()),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn ensure_emergency_stop_inactive_allows_clear_stop() {
        let active = crate::context_services::EmergencyStop::new(false);
        assert!(ensure_emergency_stop_inactive(&active).is_ok());
    }

    #[test]
    fn default_stream_system_prompt_returns_documented_default() {
        assert_eq!(
            default_stream_system_prompt(),
            "You are a helpful assistant."
        );
    }

    #[test]
    fn build_stream_prompt_formats_system_and_user_turns() {
        let prompt = build_stream_prompt("You are helpful.", "What is VAT?");
        assert!(prompt.contains("You are helpful."));
        assert!(prompt.contains("User: What is VAT?"));
        assert!(prompt.ends_with("Assistant:"));
    }

    #[test]
    fn build_stream_prompt_supports_multiline_messages() {
        let prompt = build_stream_prompt("System", "line1\nline2");
        assert!(prompt.contains("User: line1\nline2"));
    }

    #[test]
    fn build_stream_prompt_allows_empty_system_prompt() {
        let prompt = build_stream_prompt("", "hello");
        assert!(prompt.contains("User: hello"));
    }

    #[test]
    fn agent_config_from_user_agent_extracts_tools_and_model() {
        let config = agent_config_from_user_agent(&sample_user_agent(r#"["search","calculator"]"#));
        assert_eq!(config.tools, vec!["search", "calculator"]);
        assert_eq!(config.model, "fast");
        assert_eq!(config.max_tool_iterations, 5);
        assert!(config.parallel_tools);
        assert_eq!(config.system_prompt.as_deref(), Some("You are finance."));
        assert!(config.extra.is_empty());
    }

    #[test]
    fn agent_config_from_user_agent_tolerates_invalid_tools_json() {
        let config = agent_config_from_user_agent(&sample_user_agent("not-json"));
        assert!(config.tools.is_empty());
    }

    #[test]
    fn agent_config_from_user_agent_honors_parallel_tools_false() {
        let mut agent = sample_user_agent("[]");
        agent.parallel_tools = false;
        let config = agent_config_from_user_agent(&agent);
        assert!(!config.parallel_tools);
    }

    #[test]
    fn agent_config_from_user_agent_handles_missing_system_prompt() {
        let mut agent = sample_user_agent("[]");
        agent.system_prompt = None;
        let config = agent_config_from_user_agent(&agent);
        assert!(config.system_prompt.is_none());
    }

    #[test]
    fn resolve_stream_system_prompt_uses_agent_prompt_when_present() {
        let agent = sample_user_agent("[]");
        assert_eq!(
            resolve_stream_system_prompt(&agent),
            "You are finance.".to_string()
        );
    }

    #[test]
    fn resolve_stream_system_prompt_falls_back_to_default() {
        let mut agent = sample_user_agent("[]");
        agent.system_prompt = None;
        assert_eq!(
            resolve_stream_system_prompt(&agent),
            default_stream_system_prompt().to_string()
        );
    }

    #[test]
    fn build_user_memory_if_present_returns_none_when_empty() {
        assert!(build_user_memory_if_present("user-1", vec![], vec![]).is_none());
    }

    #[test]
    fn build_user_memory_if_present_returns_some_with_facts() {
        let memory =
            build_user_memory_if_present("user-1", vec![sample_fact()], vec![]).expect("memory");
        assert_eq!(memory.user_id, "user-1");
        assert_eq!(memory.facts.len(), 1);
        assert!(memory.preferences.is_empty());
    }

    #[test]
    fn build_user_memory_if_present_returns_some_with_preferences() {
        let memory = build_user_memory_if_present("user-1", vec![], vec![sample_preference()])
            .expect("memory");
        assert_eq!(memory.preferences.len(), 1);
        assert!(memory.facts.is_empty());
    }

    #[test]
    fn resolve_token_counts_uses_llm_usage_when_available() {
        let usage = crate::llm::client::TokenUsage::new(10, 20);
        let (input, output) = resolve_token_counts(Some(&usage), 100, "hello", "world");
        assert_eq!(input, 10);
        assert_eq!(output, 20);
    }

    #[test]
    fn resolve_token_counts_estimates_when_usage_missing() {
        let (input, output) = resolve_token_counts(None, 4, "hello", "world");
        assert!(input >= 4);
        assert!(output > 0);
    }

    #[test]
    fn chat_response_from_agent_output_formats_agent_label() {
        let resp =
            chat_response_from_agent_output(AgentType::Finance, "user", "ctx-9", "done".into());
        assert_eq!(resp.response, "done");
        assert_eq!(resp.context_id, "ctx-9");
        assert!(resp.agent.contains("Finance"));
        assert!(resp.agent.contains("user"));
        assert!(resp.sources.is_none());
    }

    #[test]
    fn chat_request_empty_roundtrip_json() {
        let req = ChatRequest {
            message: "ping".into(),
            agent_type: None,
            context_id: None,
            workspace_id: None,
            model: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.message, "ping");
        assert!(back.agent_type.is_none());
        assert!(back.context_id.is_none());
        assert!(back.workspace_id.is_none());
    }

    #[test]
    fn chat_request_populated_roundtrip_json() {
        let req = ChatRequest {
            message: "hi".into(),
            agent_type: Some(AgentType::Sales),
            context_id: Some("ctx-1".into()),
            workspace_id: Some("ws-9".into()),
            model: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.message, "hi");
        assert_eq!(back.agent_type, Some(AgentType::Sales));
        assert_eq!(back.context_id.as_deref(), Some("ctx-1"));
        assert_eq!(back.workspace_id.as_deref(), Some("ws-9"));
    }

    #[test]
    fn chat_stream_query_maps_to_chat_request() {
        let req: ChatRequest = ChatStreamQuery {
            message: "hello".into(),
            agent_type: Some(AgentType::Product),
            context_id: Some("ctx-1".into()),
            workspace_id: Some("ws-1".into()),
        }
        .into();

        assert_eq!(req.message, "hello");
        assert_eq!(req.agent_type, Some(AgentType::Product));
        assert_eq!(req.context_id.as_deref(), Some("ctx-1"));
        assert_eq!(req.workspace_id.as_deref(), Some("ws-1"));
    }

    #[test]
    fn chat_response_roundtrip_json() {
        let resp = ChatResponse {
            response: "answer".into(),
            agent: "finance".into(),
            context_id: "ctx-1".into(),
            sources: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.response, "answer");
        assert_eq!(back.agent, "finance");
        assert_eq!(back.context_id, "ctx-1");
        assert!(back.sources.is_none());
    }

    #[test]
    fn chat_response_serializes_sources_when_present() {
        let resp = ChatResponse {
            response: "answer".into(),
            agent: "finance (user)".into(),
            context_id: "ctx-1".into(),
            sources: Some(vec![Source {
                title: "Doc".into(),
                url: Some("https://example.com".into()),
                relevance_score: 0.9,
            }]),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["response"], "answer");
        assert!(json["sources"].is_array());
    }

    #[test]
    fn message_role_roundtrip_json() {
        for role in [
            MessageRole::System,
            MessageRole::User,
            MessageRole::Assistant,
        ] {
            let json = serde_json::to_string(&role).unwrap();
            let back: MessageRole = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{back:?}"), format!("{role:?}"));
        }
    }

    #[test]
    fn message_roundtrip_json() {
        let message = Message {
            role: MessageRole::User,
            content: "hello".into(),
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&message).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert!(matches!(back.role, MessageRole::User));
        assert_eq!(back.content, "hello");
    }

    #[test]
    fn stream_token_event_serializes_chunk() {
        let event = stream_token_event("chunk");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"token\""));
        assert!(json.contains("\"content\":\"chunk\""));
    }

    #[test]
    fn stream_start_event_serializes_metadata() {
        let event = stream_start_event(&AgentType::Product, "ctx-1");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"start\""));
        assert!(json.contains("\"context_id\":\"ctx-1\""));
        assert!(json.contains("product"));
    }

    #[test]
    fn stream_done_event_serializes_agent_and_context() {
        let event = stream_done_event(&AgentType::Sales, "system", "ctx-2");
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"done\""));
        assert!(json.contains("\"context_id\":\"ctx-2\""));
        assert!(json.contains("Sales"));
    }

    #[test]
    fn stream_event_omits_none_fields_in_json() {
        let event = StreamEvent {
            event: "token".into(),
            content: Some("hi".into()),
            agent: None,
            context_id: None,
            error: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"token\""));
        assert!(json.contains("\"content\":\"hi\""));
        assert!(!json.contains("agent"));
        assert!(!json.contains("context_id"));
        assert!(!json.contains("error"));
    }

    #[test]
    fn stream_error_event_serializes_message() {
        let event = stream_error_event("boom", Some("ctx-1"));
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"error\":\"boom\""));
        assert!(json.contains("\"context_id\":\"ctx-1\""));
    }

    #[test]
    fn stream_error_event_omits_context_when_none() {
        let event = stream_error_event("bad request", None);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"error\":\"bad request\""));
        assert!(!json.contains("context_id"));
    }
}
