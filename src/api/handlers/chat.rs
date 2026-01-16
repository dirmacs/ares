use crate::{
    agents::{registry::AgentRegistry, router::RouterAgent, Agent},
    api::handlers::user_agents::resolve_agent,
    auth::middleware::AuthUser,
    types::{
        AgentContext, AgentType, AppError, ChatRequest, ChatResponse, MessageRole, Result,
        UserMemory,
    },
    utils::toml_config::AgentConfig,
    AppState,
};
use axum::{extract::State, Json};
use uuid::Uuid;

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
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Json(payload): Json<ChatRequest>,
) -> Result<Json<ChatResponse>> {
    // Get or create conversation
    let context_id = payload
        .context_id
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Check if conversation exists, create if not
    if !state.turso.conversation_exists(&context_id).await? {
        state
            .turso
            .create_conversation(&context_id, &claims.sub, None)
            .await?;
    }
    let history = state.turso.get_conversation_history(&context_id).await?;

    // Load user memory
    let memory_facts = state.turso.get_user_memory(&claims.sub).await?;
    let preferences = state.turso.get_user_preferences(&claims.sub).await?;
    let user_memory = if !memory_facts.is_empty() || !preferences.is_empty() {
        Some(UserMemory {
            user_id: claims.sub.clone(),
            preferences,
            facts: memory_facts,
        })
    } else {
        None
    };

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
        let config = state.config_manager.config();
        let router_model = config
            .get_agent("router")
            .map(|a| a.model.as_str())
            .unwrap_or("fast");

        let router_llm = match state
            .provider_registry
            .create_client_for_model(router_model)
            .await
        {
            Ok(client) => client,
            Err(_) => state.llm_factory.create_default().await?,
        };

        let router = RouterAgent::new(router_llm);
        router.route(&payload.message, &agent_context).await?
    };

    // Execute agent
    let response = execute_agent(agent_type, &payload.message, &agent_context, &state).await?;

    // Store messages in conversation
    let msg_id = Uuid::new_v4().to_string();
    state
        .turso
        .add_message(&msg_id, &context_id, MessageRole::User, &payload.message)
        .await?;

    let resp_id = Uuid::new_v4().to_string();
    state
        .turso
        .add_message(
            &resp_id,
            &context_id,
            MessageRole::Assistant,
            &response.response,
        )
        .await?;

    Ok(Json(response))
}

async fn execute_agent(
    agent_type: AgentType,
    message: &str,
    context: &AgentContext,
    state: &AppState,
) -> Result<ChatResponse> {
    // Get agent name from type
    let agent_name = AgentRegistry::type_to_name(agent_type);

    if agent_type == AgentType::Router {
        return Err(AppError::InvalidInput(
            "Router agent cannot be called directly".to_string(),
        ));
    }

    // Resolve agent using the 3-tier hierarchy (User -> Community -> System)
    let (user_agent, source) = resolve_agent(state, &context.user_id, agent_name).await?;

    // Convert UserAgent to AgentConfig for the registry
    let config = AgentConfig {
        model: user_agent.model.clone(),
        system_prompt: user_agent.system_prompt.clone(),
        tools: user_agent.tools_vec(),
        max_tool_iterations: user_agent.max_tool_iterations as usize,
        parallel_tools: user_agent.parallel_tools,
        extra: std::collections::HashMap::new(),
    };

    // Create agent from registry using the resolved config
    let agent = state
        .agent_registry
        .create_agent_from_config(agent_name, &config)
        .await?;

    // Execute the agent
    let response = agent.execute(message, context).await?;

    Ok(ChatResponse {
        response,
        agent: format!("{:?} ({})", agent_type, source),
        context_id: context.session_id.clone(),
        sources: None,
    })
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
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
) -> Result<Json<UserMemory>> {
    let facts = state.turso.get_user_memory(&claims.sub).await?;
    let preferences = state.turso.get_user_preferences(&claims.sub).await?;

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
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Json(payload): Json<ChatRequest>,
) -> axum::response::Sse<
    impl futures::Stream<
        Item = std::result::Result<axum::response::sse::Event, std::convert::Infallible>,
    >,
> {
    use axum::response::sse::{Event, Sse};

    // Get or create conversation
    let context_id = payload
        .context_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Clone values we need for the async stream
    let state_clone = state.clone();
    let claims_clone = claims.clone();
    let message = payload.message.clone();
    let agent_type_req = payload.agent_type;
    let context_id_clone = context_id.clone();

    let stream = async_stream::stream! {
        // Setup conversation
        if !state_clone.turso.conversation_exists(&context_id_clone).await.unwrap_or(false) {
            if let Err(e) = state_clone
                .turso
                .create_conversation(&context_id_clone, &claims_clone.sub, None)
                .await {
                tracing::warn!("Failed to create conversation {}: {}", context_id_clone, e);
            }
        }

        let history = state_clone.turso.get_conversation_history(&context_id_clone).await.unwrap_or_else(|e| {
            tracing::warn!("Failed to get conversation history for {}: {}", context_id_clone, e);
            vec![]
        });

        // Load user memory
        let memory_facts = state_clone.turso.get_user_memory(&claims_clone.sub).await.unwrap_or_else(|e| {
            tracing::warn!("Failed to get user memory for {}: {}", claims_clone.sub, e);
            vec![]
        });
        let preferences = state_clone.turso.get_user_preferences(&claims_clone.sub).await.unwrap_or_else(|e| {
            tracing::warn!("Failed to get user preferences for {}: {}", claims_clone.sub, e);
            vec![]
        });
        let user_memory = if !memory_facts.is_empty() || !preferences.is_empty() {
            Some(UserMemory {
                user_id: claims_clone.sub.clone(),
                preferences,
                facts: memory_facts,
            })
        } else {
            None
        };

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
            let config = state_clone.config_manager.config();
            let router_model = config
                .get_agent("router")
                .map(|a| a.model.as_str())
                .unwrap_or("fast");

            let router_llm = match state_clone
                .provider_registry
                .create_client_for_model(router_model)
                .await
            {
                Ok(client) => client,
                Err(_) => match state_clone.llm_factory.create_default().await {
                    Ok(c) => c,
                    Err(e) => {
                        let event = StreamEvent {
                            event: "error".to_string(),
                            content: None,
                            agent: None,
                            context_id: Some(context_id_clone.clone()),
                            error: Some(format!("Failed to create LLM client: {}", e)),
                        };
                        yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                        return;
                    }
                },
            };

            let router = RouterAgent::new(router_llm);
            match router.route(&message, &agent_context).await {
                Ok(t) => t,
                Err(e) => {
                    let event = StreamEvent {
                        event: "error".to_string(),
                        content: None,
                        agent: None,
                        context_id: Some(context_id_clone.clone()),
                        error: Some(format!("Router failed: {}", e)),
                    };
                    yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                    return;
                }
            }
        };

        // Send start event
        let agent_name = AgentRegistry::type_to_name(agent_type);
        let start_event = StreamEvent {
            event: "start".to_string(),
            content: None,
            agent: Some(format!("{:?} (system)", agent_type)),
            context_id: Some(context_id_clone.clone()),
            error: None,
        };
        yield Ok(Event::default().data(serde_json::to_string(&start_event).unwrap_or_default()));

        // Resolve agent using hierarchy
        let (user_agent, source) = match crate::api::handlers::user_agents::resolve_agent(
            &state_clone,
            &claims_clone.sub,
            agent_name,
        ).await {
            Ok(r) => r,
            Err(e) => {
                let event = StreamEvent {
                    event: "error".to_string(),
                    content: None,
                    agent: None,
                    context_id: Some(context_id_clone.clone()),
                    error: Some(format!("Failed to resolve agent: {}", e)),
                };
                yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                return;
            }
        };

        // Get LLM client for streaming
        let llm = match state_clone
            .provider_registry
            .create_client_for_model(&user_agent.model)
            .await
        {
            Ok(c) => c,
            Err(_) => match state_clone.llm_factory.create_default().await {
                Ok(c) => c,
                Err(e) => {
                    let event = StreamEvent {
                        event: "error".to_string(),
                        content: None,
                        agent: None,
                        context_id: Some(context_id_clone.clone()),
                        error: Some(format!("Failed to create LLM: {}", e)),
                    };
                    yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                    return;
                }
            },
        };

        // Build the prompt with system message and history
        let system_prompt = user_agent.system_prompt.unwrap_or_else(|| "You are a helpful assistant.".to_string());
        let full_prompt = format!(
            "{}\n\nUser: {}\nAssistant:",
            system_prompt,
            message
        );

        // Stream tokens
        use futures::StreamExt;
        let mut full_response = String::new();
        match llm.stream(&full_prompt).await {
            Ok(mut token_stream) => {
                while let Some(token_result) = token_stream.next().await {
                    match token_result {
                        Ok(token) => {
                            full_response.push_str(&token);
                            let event = StreamEvent {
                                event: "token".to_string(),
                                content: Some(token),
                                agent: None,
                                context_id: None,
                                error: None,
                            };
                            yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                        }
                        Err(e) => {
                            let event = StreamEvent {
                                event: "error".to_string(),
                                content: None,
                                agent: None,
                                context_id: Some(context_id_clone.clone()),
                                error: Some(format!("Stream error: {}", e)),
                            };
                            yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                let event = StreamEvent {
                    event: "error".to_string(),
                    content: None,
                    agent: None,
                    context_id: Some(context_id_clone.clone()),
                    error: Some(format!("Failed to start stream: {}", e)),
                };
                yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
                return;
            }
        }

        // Store messages in conversation
        let msg_id = Uuid::new_v4().to_string();
        if let Err(e) = state_clone
            .turso
            .add_message(&msg_id, &context_id_clone, MessageRole::User, &message)
            .await {
            tracing::error!("Failed to store user message in conversation {}: {}", context_id_clone, e);
        }

        let resp_id = Uuid::new_v4().to_string();
        if let Err(e) = state_clone
            .turso
            .add_message(&resp_id, &context_id_clone, MessageRole::Assistant, &full_response)
            .await {
            tracing::error!("Failed to store assistant message in conversation {}: {}", context_id_clone, e);
        }

        // Send done event
        let done_event = StreamEvent {
            event: "done".to_string(),
            content: None,
            agent: Some(format!("{:?} ({})", agent_type, source)),
            context_id: Some(context_id_clone),
            error: None,
        };
        yield Ok(Event::default().data(serde_json::to_string(&done_event).unwrap_or_default()));
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    )
}
