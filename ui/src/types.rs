//! API types matching the ARES server

use serde::{Deserialize, Serialize};

/// Login request
#[derive(Debug, Clone, Serialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// Register request
#[derive(Debug, Clone, Serialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub name: String,
}

/// Authentication response
#[derive(Debug, Clone, Deserialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

/// Chat request
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

/// Chat response
#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub response: String,
    pub context_id: String,
    pub agent: String,
    #[serde(default)]
    pub sources: Option<Vec<Source>>,
}

/// Source reference in responses
#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub title: String,
    pub url: Option<String>,
    pub relevance_score: f32,
}

/// Tool call information
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ToolCallInfo {
    pub name: String,
    pub arguments: serde_json::Value,
    #[serde(default)]
    pub result: Option<String>,
}

/// Agent info from the API
#[derive(Debug, Clone, Deserialize)]
pub struct AgentInfo {
    pub agent_type: String,
    pub name: String,
    pub description: String,
}

/// Workflow info
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowInfo {
    pub name: String,
    pub entry_agent: String,
    pub fallback_agent: Option<String>,
    pub max_depth: u8,
    pub max_iterations: u8,
    pub parallel_subagents: bool,
}

/// User memory
#[derive(Debug, Clone, Deserialize)]
pub struct UserMemory {
    #[serde(default)]
    pub preferences: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub facts: Vec<String>,
}

/// Error response from API
#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    pub error: String,
    #[serde(default)]
    pub details: Option<String>,
}

/// Message in a conversation
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub agent_type: Option<String>,
    pub tool_calls: Vec<ToolCallInfo>,
    pub is_streaming: bool,
}

/// Message role
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role: MessageRole::User,
            content: content.into(),
            timestamp: chrono::Utc::now(),
            agent_type: None,
            tool_calls: vec![],
            is_streaming: false,
        }
    }

    pub fn assistant(content: impl Into<String>, agent_type: Option<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role: MessageRole::Assistant,
            content: content.into(),
            timestamp: chrono::Utc::now(),
            agent_type,
            tool_calls: vec![],
            is_streaming: false,
        }
    }

    pub fn streaming(agent_type: Option<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role: MessageRole::Assistant,
            content: String::new(),
            timestamp: chrono::Utc::now(),
            agent_type,
            tool_calls: vec![],
            is_streaming: true,
        }
    }
}

/// Conversation state
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    pub id: Option<String>,
    pub messages: Vec<Message>,
    pub selected_agent: Option<String>,
}

/// Streaming event from the chat/stream endpoint
#[derive(Debug, Clone, Deserialize)]
pub struct StreamEvent {
    /// Event type: "start", "token", "done", "error"
    pub event: String,
    /// Token content (for "token" events)
    #[serde(default)]
    pub content: Option<String>,
    /// Agent type that handled the request
    #[serde(default)]
    pub agent: Option<String>,
    /// Context ID for the conversation
    #[serde(default)]
    pub context_id: Option<String>,
    /// Error message (for "error" events)
    #[serde(default)]
    pub error: Option<String>,
}
