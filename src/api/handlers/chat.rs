use crate::{
    AppState,
    agents::router::RouterAgent,
    auth::middleware::AuthUser,
    types::{
        AgentContext, AgentType, AppError, ChatRequest, ChatResponse, MessageRole, Result,
        UserMemory,
    },
};
use axum::{Json, extract::State};
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
        // Use router agent to determine appropriate agent
        let router = RouterAgent::new(state.llm_factory.create_default().await?);
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
    use crate::agents::*;

    let llm_client = state.llm_factory.create_default().await?;

    let response = match agent_type {
        AgentType::Product => {
            let agent = product::ProductAgent::new(llm_client);
            agent.execute(message, context).await?
        }
        AgentType::Invoice => {
            let agent = invoice::InvoiceAgent::new(llm_client);
            agent.execute(message, context).await?
        }
        AgentType::Sales => {
            let agent = sales::SalesAgent::new(llm_client);
            agent.execute(message, context).await?
        }
        AgentType::Finance => {
            let agent = finance::FinanceAgent::new(llm_client);
            agent.execute(message, context).await?
        }
        AgentType::HR => {
            let agent = hr::HrAgent::new(llm_client);
            agent.execute(message, context).await?
        }
        AgentType::Orchestrator => {
            let agent = orchestrator::OrchestratorAgent::new(llm_client, state.clone());
            agent.execute(message, context).await?
        }
        AgentType::Router => {
            return Err(AppError::InvalidInput(
                "Router agent cannot be called directly".to_string(),
            ));
        }
    };

    Ok(ChatResponse {
        response,
        agent: format!("{:?}", agent_type),
        context_id: context.session_id.clone(),
        sources: None,
    })
}

/// Get user memory
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
