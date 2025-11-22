use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// ============= API Request/Response Types =============

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ChatRequest {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<AgentType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ChatResponse {
    pub response: String,
    pub agent: String,
    pub context_id: String,
    pub sources: Option<Vec<Source>>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct Source {
    pub title: String,
    pub url: Option<String>,
    pub relevance_score: f32,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ResearchRequest {
    pub query: String,
    pub depth: Option<u8>,
    pub max_iterations: Option<u8>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ResearchResponse {
    pub findings: String,
    pub sources: Vec<Source>,
    pub duration_ms: u64,
}

// ============= Agent Types =============

#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    Router,
    Orchestrator,
    Product,
    Invoice,
    Sales,
    Finance,
    HR,
}

#[derive(Debug, Clone)]
pub struct AgentContext {
    pub user_id: String,
    pub session_id: String,
    pub conversation_history: Vec<Message>,
    pub user_memory: Option<UserMemory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

// ============= Memory Types =============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMemory {
    pub user_id: String,
    pub preferences: Vec<Preference>,
    pub facts: Vec<MemoryFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    pub category: String,
    pub key: String,
    pub value: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    pub id: String,
    pub user_id: String,
    pub category: String,
    pub fact_key: String,
    pub fact_value: String,
    pub confidence: f32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ============= Tool Types =============

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub result: serde_json::Value,
}

// ============= RAG Types =============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub content: String,
    pub metadata: DocumentMetadata,
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub title: String,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub query: String,
    pub limit: usize,
    pub threshold: f32,
    pub filters: Option<Vec<SearchFilter>>,
}

#[derive(Debug, Clone)]
pub struct SearchFilter {
    pub field: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub document: Document,
    pub score: f32,
}

// ============= Authentication Types =============

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub email: String,
    pub exp: usize,
    pub iat: usize,
}

// ============= Error Types =============

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("LLM error: {0}")]
    LLM(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl axum::response::IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            AppError::Database(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::LLM(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::Auth(msg) => (axum::http::StatusCode::UNAUTHORIZED, msg),
            AppError::NotFound(msg) => (axum::http::StatusCode::NOT_FOUND, msg),
            AppError::InvalidInput(msg) => (axum::http::StatusCode::BAD_REQUEST, msg),
            AppError::Internal(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg),
        };

        let body = serde_json::json!({
            "error": message
        });

        (status, axum::Json(body)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
