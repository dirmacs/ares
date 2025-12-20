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
    pub token_type: String,
    pub expires_in: u64,
    pub user_id: String,
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
    pub agent_type: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallInfo>,
    #[serde(default)]
    pub sources: Vec<String>,
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
    pub name: String,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Agents list response
#[derive(Debug, Clone, Deserialize)]
pub struct AgentsListResponse {
    pub agents: Vec<AgentInfo>,
}

/// Workflow info
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowInfo {
    pub name: String,
    pub description: String,
    pub agents: Vec<String>,
    pub max_depth: usize,
}

/// Workflows list response
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowsListResponse {
    pub workflows: Vec<WorkflowInfo>,
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
