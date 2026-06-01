use crate::{
    agents::{registry::AgentRegistry, router::RouterAgent, Agent},
    api::handlers::user_agents::resolve_agent,
    auth::middleware::AuthUser,
    db::agent_runs,
    memory::estimate_tokens,
    types::{
        AgentContext, AgentType, AppError, ChatRequest, ChatResponse, MessageRole, Result,
        UserMemory,
    },
    utils::toml_config::AgentConfig,
    AppState,
};
use axum::{extract::State, response::Response, Extension, Json};
use crate::db::postgres::UserAgent;
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

/// Converts a resolved user agent record into runtime agent configuration.
fn agent_config_from_user_agent(user_agent: &UserAgent) -> AgentConfig {
    AgentConfig {
        model: user_agent.model.clone(),
        system_prompt: user_agent.system_prompt.clone(),
        tools: user_agent.tools_vec(),
        max_tool_iterations: user_agent.max_tool_iterations as usize,
        parallel_tools: user_agent.parallel_tools,
        extra: std::collections::HashMap::new(),
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
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    tenant_ctx: Option<Extension<crate::models::TenantContext>>,
    Json(payload): Json<ChatRequest>,
) -> Result<Response> {
    validate_chat_request(&payload)?;

    // Get or create conversation
    let context_id = resolve_context_id(payload.context_id.as_ref());

    // Check if conversation exists, create if not
    if !state.db.conversation_exists(&context_id).await? {
        state
            .db
            .create_conversation(&context_id, &claims.sub, None)
            .await?;
    }
    let history = state.db.get_conversation_history(&context_id).await?;
    // Compute history token estimate in the same pass (before clone into AgentContext)
    let history_input_tokens: usize = history.iter().map(|m| estimate_tokens(&m.content)).sum();

    // Load user memory
    let memory_facts = state.db.get_user_memory(&claims.sub).await?;
    let preferences = state.db.get_user_preferences(&claims.sub).await?;
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

    // Execute agent with timing
    let agent_name_for_run = AgentRegistry::type_to_name(&agent_type).to_string();
    let start = std::time::Instant::now();
    let (response, usage) =
        execute_agent(agent_type, &payload.message, &agent_context, &state).await?;
    let duration_ms = start.elapsed().as_millis() as i64;

    // Store messages in conversation
    let msg_id = Uuid::new_v4().to_string();
    state
        .db
        .add_message(&msg_id, &context_id, MessageRole::User, &payload.message)
        .await?;

    let resp_id = Uuid::new_v4().to_string();
    state
        .db
        .add_message(
            &resp_id,
            &context_id,
            MessageRole::Assistant,
            &response.response,
        )
        .await?;

    // Use actual LLM token counts; fall back to heuristic estimates if unavailable
    let (input_tokens, output_tokens) = if let Some(u) = usage {
        (u.prompt_tokens, u.completion_tokens)
    } else {
        (
            (history_input_tokens + estimate_tokens(&payload.message)) as u32,
            estimate_tokens(&response.response) as u32,
        )
    };

    // Record agent run (fire-and-forget)
    {
        let pool = state.tenant_db.pool().clone();
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
    state: &AppState,
) -> Result<(ChatResponse, Option<crate::llm::client::TokenUsage>)> {
    // Get agent name from type
    let agent_name = AgentRegistry::type_to_name(&agent_type);

    ensure_not_direct_router(&agent_type)?;

    // Resolve agent using the 3-tier hierarchy (User -> Community -> System)
    let (user_agent, source) =
        resolve_agent(state, &context.user_id, agent_name.to_string()).await?;

    let config = agent_config_from_user_agent(&user_agent);

    // Create agent from registry using the resolved config
    let agent = state
        .agent_registry
        .create_agent_from_config(agent_name, &config)
        .await?;

    // Execute the agent
    let agent_resp = agent.execute(message, context).await?;

    Ok((
        ChatResponse {
            response: agent_resp.content,
            agent: format!("{:?} ({})", agent_type, source),
            context_id: context.session_id.clone(),
            sources: None,
        },
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
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
) -> Result<Json<UserMemory>> {
    let facts = state.db.get_user_memory(&claims.sub).await?;
    let preferences = state.db.get_user_preferences(&claims.sub).await?;

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

    let validation_error = validate_chat_request(&payload).err();

    // Get or create conversation
    let context_id = resolve_context_id(payload.context_id.as_ref());

    // Clone values we need for the async stream
    let state_clone = state.clone();
    let claims_clone = claims.clone();
    let message = payload.message.clone();
    let agent_type_req = payload.agent_type;
    let runtime_workspace_id = payload.workspace_id.clone();
    let context_id_clone = context_id.clone();

    let stream = async_stream::stream! {
        if let Some(e) = &validation_error {
            let event = StreamEvent {
                event: "error".to_string(),
                content: None,
                agent: None,
                context_id: None,
                error: Some(e.to_string()),
            };
            yield Ok(Event::default().data(serde_json::to_string(&event).unwrap_or_default()));
            return;
        }

        // Setup conversation
        if !state_clone.db.conversation_exists(&context_id_clone).await.unwrap_or(false) {
            if let Err(e) = state_clone
                .db
                .create_conversation(&context_id_clone, &claims_clone.sub, None)
                .await {
                tracing::warn!("Failed to create conversation {}: {}", context_id_clone, e);
            }
        }

        let history = state_clone.db.get_conversation_history(&context_id_clone).await.unwrap_or_else(|e| {
            tracing::warn!("Failed to get conversation history for {}: {}", context_id_clone, e);
            vec![]
        });

        // Load user memory
        let memory_facts = state_clone.db.get_user_memory(&claims_clone.sub).await.unwrap_or_else(|e| {
            tracing::warn!("Failed to get user memory for {}: {}", claims_clone.sub, e);
            vec![]
        });
        let preferences = state_clone.db.get_user_preferences(&claims_clone.sub).await.unwrap_or_else(|e| {
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
        let agent_name = AgentRegistry::type_to_name(&agent_type);
        let start_event = StreamEvent {
            event: "start".to_string(),
            content: None,
            agent: Some(format!("{} (system)", agent_type)),
            context_id: Some(context_id_clone.clone()),
            error: None,
        };
        yield Ok(Event::default().data(serde_json::to_string(&start_event).unwrap_or_default()));

        // Resolve agent using hierarchy
        let (user_agent, source) = match crate::api::handlers::user_agents::resolve_agent(
            &state_clone,
            &claims_clone.sub,
            agent_name.to_string(),
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
        let system_prompt = user_agent
            .system_prompt
            .clone()
            .unwrap_or_else(|| default_stream_system_prompt().to_string());
        let full_prompt = build_stream_prompt(&system_prompt, &message);

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
            .db
            .add_message(&msg_id, &context_id_clone, MessageRole::User, &message)
            .await {
            tracing::error!("Failed to store user message in conversation {}: {}", context_id_clone, e);
        }

        let resp_id = Uuid::new_v4().to_string();
        if let Err(e) = state_clone
            .db
            .add_message(&resp_id, &context_id_clone, MessageRole::Assistant, &full_response)
            .await {
            tracing::error!("Failed to store assistant message in conversation {}: {}", context_id_clone, e);
        }

        // Record agent run for billing (streaming calls were previously invisible)
        {
            let pool = state_clone.tenant_db.pool().clone();
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
use axum::response::IntoResponse;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Source;

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

    #[test]
    fn validate_chat_request_rejects_empty_message() {
        let payload = ChatRequest {
            message: "   ".into(),
            agent_type: None,
            context_id: None,
            workspace_id: None,
        };
        let err = validate_chat_request(&payload).unwrap_err();
        match err {
            AppError::InvalidInput(msg) => assert!(msg.contains("empty")),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn validate_chat_request_accepts_non_empty_message() {
        let payload = ChatRequest {
            message: "hello".into(),
            agent_type: None,
            context_id: None,
            workspace_id: None,
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
    fn ensure_not_direct_router_rejects_router_type() {
        let err = ensure_not_direct_router(&AgentType::Router).unwrap_err();
        match err {
            AppError::InvalidInput(msg) => assert!(msg.contains("Router")),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn ensure_not_direct_router_allows_builtin_agents() {
        assert!(ensure_not_direct_router(&AgentType::Finance).is_ok());
    }

    #[test]
    fn build_stream_prompt_formats_system_and_user_turns() {
        let prompt = build_stream_prompt("You are helpful.", "What is VAT?");
        assert!(prompt.contains("You are helpful."));
        assert!(prompt.contains("User: What is VAT?"));
        assert!(prompt.ends_with("Assistant:"));
    }

    #[test]
    fn agent_config_from_user_agent_extracts_tools() {
        let config = agent_config_from_user_agent(&sample_user_agent(
            r#"["search","calculator"]"#,
        ));
        assert_eq!(config.tools, vec!["search", "calculator"]);
        assert_eq!(config.model, "fast");
        assert_eq!(config.max_tool_iterations, 5);
        assert!(config.parallel_tools);
    }

    #[test]
    fn agent_config_from_user_agent_tolerates_invalid_tools_json() {
        let config = agent_config_from_user_agent(&sample_user_agent("not-json"));
        assert!(config.tools.is_empty());
    }

    #[test]
    fn chat_request_roundtrip_json() {
        let req = ChatRequest {
            message: "hi".into(),
            agent_type: Some(AgentType::Sales),
            context_id: Some("ctx-1".into()),
            workspace_id: Some("ws-9".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.message, "hi");
        assert_eq!(back.agent_type, Some(AgentType::Sales));
        assert_eq!(back.context_id.as_deref(), Some("ctx-1"));
        assert_eq!(back.workspace_id.as_deref(), Some("ws-9"));
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
    fn stream_event_error_serializes_message() {
        let event = StreamEvent {
            event: "error".into(),
            content: None,
            agent: None,
            context_id: Some("ctx-1".into()),
            error: Some("boom".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"error\":\"boom\""));
        assert!(json.contains("\"context_id\":\"ctx-1\""));
    }
}
