//! Conversation management handlers.
//!
//! This module provides CRUD operations for user conversations.

use std::sync::Arc;
use crate::AppState;
use ares_cordis_core::Context;

use crate::{
    auth::middleware::AuthUser,
    db::postgres::Conversation,
    db::traits::ConversationSummary as DbConversationSummary,
    types::{AppError, Message, MessageRole, Result},
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

/// Maps a database list-row into the API summary shape.
fn summary_from_list_row(c: DbConversationSummary) -> ConversationSummary {
    ConversationSummary {
        id: c.id,
        title: Some(c.title),
        message_count: c.message_count,
        created_at: c.created_at,
        updated_at: c.updated_at,
    }
}

/// Maps a stored message into the API message shape.
fn message_to_api(conversation_id: &str, idx: usize, msg: Message) -> ConversationMessage {
    ConversationMessage {
        id: format!("{conversation_id}-{idx}"),
        role: message_role_to_api(&msg.role),
        content: msg.content,
        created_at: msg.timestamp.to_rfc3339(),
    }
}

/// Lowercases the debug representation of [`MessageRole`] for API consumers.
fn message_role_to_api(role: &MessageRole) -> String {
    format!("{role:?}").to_lowercase()
}

/// Verifies the authenticated user owns the conversation.
fn ensure_conversation_owner(conversation_user_id: &str, claims_sub: &str, action: &str) -> Result<()> {
    if conversation_user_id != claims_sub {
        return Err(AppError::Auth(format!(
            "Not authorized to {action} this conversation"
        )));
    }
    Ok(())
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
#[derive(Debug, Deserialize, Serialize, ToSchema)]
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
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
) -> Result<Json<Vec<ConversationSummary>>> {
    let conversations = ctx.get::<crate::context_services::DbService>().expect("not provided").0.get_user_conversations(&claims.sub).await?;

    let summaries: Vec<ConversationSummary> = conversations
        .into_iter()
        .map(summary_from_list_row)
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
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Path(id): Path<String>,
) -> Result<Json<ConversationDetails>> {
    // Verify conversation belongs to user
    let conversation = ctx.get::<crate::context_services::DbService>().expect("not provided").0.get_conversation(&id).await?;

    ensure_conversation_owner(&conversation.user_id, &claims.sub, "access")?;

    let messages = ctx.get::<crate::context_services::DbService>().expect("not provided").0.get_conversation_history(&id).await?;

    let message_details: Vec<ConversationMessage> = messages
        .into_iter()
        .enumerate()
        .map(|(idx, msg)| message_to_api(&id, idx, msg))
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
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Path(id): Path<String>,
    Json(payload): Json<UpdateConversationRequest>,
) -> Result<Json<serde_json::Value>> {
    // Verify conversation belongs to user
    let conversation = ctx.get::<crate::context_services::DbService>().expect("not provided").0.get_conversation(&id).await?;

    ensure_conversation_owner(&conversation.user_id, &claims.sub, "modify")?;

    ctx.get::<crate::context_services::DbService>().expect("not provided").0
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
    State(ctx): State<Arc<Context>>,
    AuthUser(claims): AuthUser,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode> {
    // Verify conversation belongs to user
    let conversation = ctx.get::<crate::context_services::DbService>().expect("not provided").0.get_conversation(&id).await?;

    ensure_conversation_owner(&conversation.user_id, &claims.sub, "delete")?;

    ctx.get::<crate::context_services::DbService>().expect("not provided").0.delete_conversation(&id).await?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn conversation_summary_from_postgres_row() {
        let summary = ConversationSummary::from(Conversation {
            id: "conv-1".to_string(),
            user_id: "user-1".to_string(),
            title: Some("Notes".to_string()),
            message_count: 3,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
        });
        assert_eq!(summary.id, "conv-1");
        assert_eq!(summary.title.as_deref(), Some("Notes"));
        assert_eq!(summary.message_count, 3);
    }

    #[test]
    fn summary_from_list_row_wraps_title() {
        let summary = summary_from_list_row(DbConversationSummary {
            id: "c1".to_string(),
            title: "My chat".to_string(),
            created_at: "t0".to_string(),
            updated_at: "t1".to_string(),
            message_count: 2,
        });
        assert_eq!(summary.title.as_deref(), Some("My chat"));
        assert_eq!(summary.message_count, 2);
    }

    #[test]
    fn message_role_to_api_lowercases_debug_form() {
        assert_eq!(message_role_to_api(&MessageRole::Assistant), "assistant");
        assert_eq!(message_role_to_api(&MessageRole::User), "user");
        assert_eq!(message_role_to_api(&MessageRole::System), "system");
    }

    #[test]
    fn message_to_api_builds_pseudo_id_and_timestamp() {
        let msg = Message {
            role: MessageRole::User,
            content: "hello".to_string(),
            timestamp: Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0).unwrap(),
        };
        let api = message_to_api("conv-9", 1, msg);
        assert_eq!(api.id, "conv-9-1");
        assert_eq!(api.role, "user");
        assert_eq!(api.content, "hello");
        assert!(api.created_at.contains("2024"));
    }

    #[test]
    fn ensure_conversation_owner_rejects_foreign_user() {
        let err = ensure_conversation_owner("owner", "other", "access").unwrap_err();
        assert!(matches!(err, AppError::Auth(_)));
        assert!(err.to_string().contains("Not authorized to access"));
    }

    #[test]
    fn ensure_conversation_owner_accepts_owner() {
        assert!(ensure_conversation_owner("owner", "owner", "modify").is_ok());
    }

    #[test]
    fn update_conversation_request_serde_roundtrip() {
        let req = UpdateConversationRequest {
            title: Some("Renamed".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: UpdateConversationRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title.as_deref(), Some("Renamed"));
    }

    #[test]
    fn update_conversation_request_clear_title_deserializes_null() {
        let parsed: UpdateConversationRequest =
            serde_json::from_str(r#"{"title":null}"#).unwrap();
        assert!(parsed.title.is_none());
    }

    #[test]
    fn conversation_details_serializes_messages() {
        let details = ConversationDetails {
            id: "c1".to_string(),
            title: None,
            messages: vec![ConversationMessage {
                id: "c1-0".to_string(),
                role: "user".to_string(),
                content: "hi".to_string(),
                created_at: "2024-01-01T00:00:00Z".to_string(),
            }],
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&details).unwrap();
        assert!(json.contains("\"messages\""));
        assert!(json.contains("\"hi\""));
    }

    #[test]
    fn ensure_conversation_owner_does_not_depend_on_env() {
        std::env::remove_var("DATABASE_URL");
        assert!(ensure_conversation_owner("u1", "u1", "delete").is_ok());
    }

    #[test]
    fn conversation_summary_serializes_optional_title() {
        let summary = ConversationSummary {
            id: "c2".to_string(),
            title: Some("Title".to_string()),
            message_count: 0,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"title\":\"Title\""));
        assert!(json.contains("\"message_count\":0"));
    }

    #[test]
    fn ensure_conversation_owner_error_includes_action() {
        let err = ensure_conversation_owner("owner", "intruder", "delete").unwrap_err();
        assert!(err.to_string().contains("delete"));
    }

    #[test]
    fn conversation_summary_from_postgres_row_none_title() {
        let summary = ConversationSummary::from(Conversation {
            id: "conv-2".to_string(),
            user_id: "user-1".to_string(),
            title: None,
            message_count: 0,
            created_at: "2024-03-01T00:00:00Z".to_string(),
            updated_at: "2024-03-02T00:00:00Z".to_string(),
        });
        assert_eq!(summary.id, "conv-2");
        assert!(summary.title.is_none());
        assert_eq!(summary.created_at, "2024-03-01T00:00:00Z");
        assert_eq!(summary.updated_at, "2024-03-02T00:00:00Z");
    }

    #[test]
    fn summary_from_list_row_maps_id_and_timestamps() {
        let summary = summary_from_list_row(DbConversationSummary {
            id: "list-row-1".to_string(),
            title: "Daily standup".to_string(),
            created_at: "2024-04-01T08:00:00Z".to_string(),
            updated_at: "2024-04-01T09:30:00Z".to_string(),
            message_count: 5,
        });
        assert_eq!(summary.id, "list-row-1");
        assert_eq!(summary.created_at, "2024-04-01T08:00:00Z");
        assert_eq!(summary.updated_at, "2024-04-01T09:30:00Z");
        assert_eq!(summary.message_count, 5);
    }

    #[test]
    fn conversation_summary_serializes_null_title() {
        let summary = ConversationSummary {
            id: "c3".to_string(),
            title: None,
            message_count: 1,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"title\":null"));
        assert!(json.contains("\"id\":\"c3\""));
    }

    #[test]
    fn conversation_message_serializes_role_content_and_timestamp() {
        let msg = ConversationMessage {
            id: "c1-2".to_string(),
            role: "assistant".to_string(),
            content: "reply".to_string(),
            created_at: "2024-05-01T12:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"assistant\""));
        assert!(json.contains("\"content\":\"reply\""));
        assert!(json.contains("\"created_at\":\"2024-05-01T12:00:00Z\""));
    }

    #[test]
    fn message_to_api_index_zero_and_assistant_role() {
        let msg = Message {
            role: MessageRole::Assistant,
            content: "done".to_string(),
            timestamp: Utc.with_ymd_and_hms(2024, 7, 4, 9, 30, 0).unwrap(),
        };
        let api = message_to_api("thread-1", 0, msg);
        assert_eq!(api.id, "thread-1-0");
        assert_eq!(api.role, "assistant");
        assert_eq!(api.content, "done");
        assert!(api.created_at.contains("2024-07-04"));
    }

    #[test]
    fn message_to_api_system_role() {
        let msg = Message {
            role: MessageRole::System,
            content: "be helpful".to_string(),
            timestamp: Utc.with_ymd_and_hms(2024, 8, 15, 0, 0, 0).unwrap(),
        };
        let api = message_to_api("sys-conv", 3, msg);
        assert_eq!(api.id, "sys-conv-3");
        assert_eq!(api.role, "system");
    }

    #[test]
    fn ensure_conversation_owner_modify_denied_mentions_modify() {
        let err = ensure_conversation_owner("alice", "bob", "modify").unwrap_err();
        assert!(err.to_string().contains("modify"));
        assert!(err.to_string().contains("Not authorized"));
    }

    #[test]
    fn update_conversation_request_empty_object_leaves_title_none() {
        let parsed: UpdateConversationRequest = serde_json::from_str("{}").unwrap();
        assert!(parsed.title.is_none());
    }

    #[test]
    fn conversation_details_serializes_empty_messages() {
        let details = ConversationDetails {
            id: "empty".to_string(),
            title: Some("Untitled".to_string()),
            messages: vec![],
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&details).unwrap();
        assert!(json.contains("\"messages\":[]"));
        assert!(json.contains("\"title\":\"Untitled\""));
        assert!(json.contains("\"created_at\""));
        assert!(json.contains("\"updated_at\""));
    }
}
