//! Conversation management handlers.
//!
//! This module provides CRUD operations for user conversations.

use crate::{
    auth::middleware::AuthUser,
    db::turso::Conversation,
    types::{AppError, Result},
    AppState,
};
use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Conversation summary returned in list endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationSummary {
    /// Unique conversation identifier
    pub id: String,
    /// Optional conversation title
    pub title: Option<String>,
    /// Number of messages in the conversation
    pub message_count: i32,
    /// RFC3339 formatted creation timestamp
    pub created_at: String,
    /// RFC3339 formatted last update timestamp
    pub updated_at: String,
}

impl From<Conversation> for ConversationSummary {
    fn from(c: Conversation) -> Self {
        Self {
            id: c.id,
            title: c.title,
            message_count: c.message_count,
            created_at: c.created_at,
            updated_at: c.updated_at,
        }
    }
}

/// Full conversation with messages.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationDetails {
    /// Unique conversation identifier
    pub id: String,
    /// Optional conversation title
    pub title: Option<String>,
    /// Messages in the conversation, ordered by time
    pub messages: Vec<ConversationMessage>,
    /// RFC3339 formatted creation timestamp
    pub created_at: String,
    /// RFC3339 formatted last update timestamp
    pub updated_at: String,
}

/// A message in a conversation.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConversationMessage {
    /// Unique message identifier
    pub id: String,
    /// Message role: "user", "assistant", or "system"
    pub role: String,
    /// Message content
    pub content: String,
    /// RFC3339 formatted timestamp
    pub created_at: String,
}

/// Request to update a conversation.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateConversationRequest {
    /// New title for the conversation (None to clear)
    pub title: Option<String>,
}

/// List all conversations for the authenticated user.
#[utoipa::path(
    get,
    path = "/api/conversations",
    responses(
        (status = 200, description = "List of conversations", body = Vec<ConversationSummary>),
        (status = 401, description = "Unauthorized")
    ),
    tag = "conversations",
    security(("bearer" = []))
)]
pub async fn list_conversations(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
) -> Result<Json<Vec<ConversationSummary>>> {
    let conversations = state.turso.get_user_conversations(&claims.sub).await?;

    let summaries: Vec<ConversationSummary> = conversations
        .into_iter()
        .map(ConversationSummary::from)
        .collect();

    Ok(Json(summaries))
}

/// Get a specific conversation with all messages.
#[utoipa::path(
    get,
    path = "/api/conversations/{id}",
    params(
        ("id" = String, Path, description = "Conversation ID")
    ),
    responses(
        (status = 200, description = "Conversation details", body = ConversationDetails),
        (status = 404, description = "Conversation not found"),
        (status = 401, description = "Unauthorized")
    ),
    tag = "conversations",
    security(("bearer" = []))
)]
pub async fn get_conversation(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(id): Path<String>,
) -> Result<Json<ConversationDetails>> {
    // Verify conversation belongs to user
    let conversation = state.turso.get_conversation(&id).await?;

    if conversation.user_id != claims.sub {
        return Err(AppError::Auth(
            "Not authorized to access this conversation".to_string(),
        ));
    }

    let messages = state.turso.get_conversation_history(&id).await?;

    let message_details: Vec<ConversationMessage> = messages
        .into_iter()
        .enumerate()
        .map(|(idx, msg)| ConversationMessage {
            id: format!("{}-{}", id, idx), // Generate a pseudo-ID from conversation_id and index
            role: format!("{:?}", msg.role).to_lowercase(),
            content: msg.content,
            created_at: msg.timestamp.to_rfc3339(),
        })
        .collect();

    Ok(Json(ConversationDetails {
        id: conversation.id,
        title: conversation.title,
        messages: message_details,
        created_at: conversation.created_at,
        updated_at: conversation.updated_at,
    }))
}

/// Update a conversation (e.g., change title).
#[utoipa::path(
    put,
    path = "/api/conversations/{id}",
    params(
        ("id" = String, Path, description = "Conversation ID")
    ),
    request_body = UpdateConversationRequest,
    responses(
        (status = 200, description = "Conversation updated"),
        (status = 404, description = "Conversation not found"),
        (status = 401, description = "Unauthorized")
    ),
    tag = "conversations",
    security(("bearer" = []))
)]
pub async fn update_conversation(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(id): Path<String>,
    Json(payload): Json<UpdateConversationRequest>,
) -> Result<Json<serde_json::Value>> {
    // Verify conversation belongs to user
    let conversation = state.turso.get_conversation(&id).await?;

    if conversation.user_id != claims.sub {
        return Err(AppError::Auth(
            "Not authorized to modify this conversation".to_string(),
        ));
    }

    state
        .turso
        .update_conversation_title(&id, payload.title.as_deref())
        .await?;

    Ok(Json(serde_json::json!({"success": true})))
}

/// Delete a conversation and all its messages.
#[utoipa::path(
    delete,
    path = "/api/conversations/{id}",
    params(
        ("id" = String, Path, description = "Conversation ID")
    ),
    responses(
        (status = 204, description = "Conversation deleted"),
        (status = 404, description = "Conversation not found"),
        (status = 401, description = "Unauthorized")
    ),
    tag = "conversations",
    security(("bearer" = []))
)]
pub async fn delete_conversation(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode> {
    // Verify conversation belongs to user
    let conversation = state.turso.get_conversation(&id).await?;

    if conversation.user_id != claims.sub {
        return Err(AppError::Auth(
            "Not authorized to delete this conversation".to_string(),
        ));
    }

    state.turso.delete_conversation(&id).await?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}
