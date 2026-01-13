//! Core types used throughout the A.R.E.S server.
//!
//! This module contains all the common data structures used for:
//! - API requests and responses
//! - Agent configuration and context
//! - Memory and user preferences
//! - Tool definitions and calls
//! - RAG (Retrieval Augmented Generation)
//! - Authentication
//! - Error handling

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Default datetime for serde deserialization
fn default_datetime() -> DateTime<Utc> {
    Utc::now()
}

// ============= API Request/Response Types =============

/// Request payload for chat endpoints.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ChatRequest {
    /// The user's message to send to the agent.
    pub message: String,
    /// Optional agent type to handle the request. Defaults to router.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<AgentType>,
    /// Optional context ID for conversation continuity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
}

/// Response from chat endpoints.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ChatResponse {
    /// The agent's response text.
    pub response: String,
    /// The name of the agent that handled the request.
    pub agent: String,
    /// Context ID for continuing this conversation.
    pub context_id: String,
    /// Optional sources used to generate the response.
    pub sources: Option<Vec<Source>>,
}

/// A source reference used in responses.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone)]
pub struct Source {
    /// Title of the source document or webpage.
    pub title: String,
    /// URL of the source, if available.
    pub url: Option<String>,
    /// Relevance score (0.0 to 1.0) indicating how relevant this source is.
    pub relevance_score: f32,
}

/// Request payload for deep research endpoints.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ResearchRequest {
    /// The research query or question.
    pub query: String,
    /// Optional maximum depth for recursive research (default: 3).
    pub depth: Option<u8>,
    /// Optional maximum iterations across all agents (default: 10).
    pub max_iterations: Option<u8>,
}

/// Response from deep research endpoints.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ResearchResponse {
    /// The compiled research findings.
    pub findings: String,
    /// Sources discovered during research.
    pub sources: Vec<Source>,
    /// Time taken for the research in milliseconds.
    pub duration_ms: u64,
}

// ============= RAG API Types =============

/// Request to ingest a document into the RAG system.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RagIngestRequest {
    /// Collection name to ingest into.
    pub collection: String,
    /// The text content to ingest.
    pub content: String,
    /// Optional document title.
    pub title: Option<String>,
    /// Optional source URL or path.
    pub source: Option<String>,
    /// Optional tags for categorization.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Chunking strategy to use.
    #[serde(default)]
    pub chunking_strategy: Option<String>,
}

/// Response from document ingestion.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RagIngestResponse {
    /// Number of chunks created.
    pub chunks_created: usize,
    /// Document IDs created.
    pub document_ids: Vec<String>,
    /// Collection name.
    pub collection: String,
}

/// Request to search the RAG system.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RagSearchRequest {
    /// Collection to search.
    pub collection: String,
    /// The search query.
    pub query: String,
    /// Maximum results to return (default: 10).
    #[serde(default = "default_search_limit")]
    pub limit: usize,
    /// Search strategy to use: semantic, bm25, fuzzy, hybrid.
    #[serde(default)]
    pub strategy: Option<String>,
    /// Minimum similarity threshold (0.0 to 1.0).
    #[serde(default = "default_search_threshold")]
    pub threshold: f32,
    /// Whether to enable reranking.
    #[serde(default)]
    pub rerank: bool,
    /// Reranker model to use if reranking.
    #[serde(default)]
    pub reranker_model: Option<String>,
}

fn default_search_limit() -> usize {
    10
}

fn default_search_threshold() -> f32 {
    0.0
}

/// Single search result.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RagSearchResult {
    /// Document ID.
    pub id: String,
    /// Matching text content.
    pub content: String,
    /// Relevance score.
    pub score: f32,
    /// Document metadata.
    pub metadata: DocumentMetadata,
}

/// Response from RAG search.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RagSearchResponse {
    /// Search results.
    pub results: Vec<RagSearchResult>,
    /// Total number of results before limit.
    pub total: usize,
    /// Search strategy used.
    pub strategy: String,
    /// Whether reranking was applied.
    pub reranked: bool,
    /// Query processing time in milliseconds.
    pub duration_ms: u64,
}

/// Request to delete a collection.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RagDeleteCollectionRequest {
    /// Collection name to delete.
    pub collection: String,
}

/// Response from collection deletion.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RagDeleteCollectionResponse {
    /// Whether deletion was successful.
    pub success: bool,
    /// Collection that was deleted.
    pub collection: String,
    /// Number of documents deleted.
    pub documents_deleted: usize,
}

// ============= Workflow Types =============

/// Request payload for workflow execution endpoints.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct WorkflowRequest {
    /// The query to process through the workflow.
    pub query: String,
    /// Additional context data as key-value pairs.
    #[serde(default)]
    pub context: std::collections::HashMap<String, serde_json::Value>,
}

// ============= Agent Types =============

/// Available agent types in the system.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    /// Routes requests to appropriate specialized agents.
    Router,
    /// Orchestrates complex multi-step tasks.
    Orchestrator,
    /// Handles product-related queries.
    Product,
    /// Handles invoice and billing queries.
    Invoice,
    /// Handles sales-related queries.
    Sales,
    /// Handles financial queries and analysis.
    Finance,
    /// Handles HR and employee-related queries.
    HR,
}

/// Context passed to agents during request processing.
#[derive(Debug, Clone)]
pub struct AgentContext {
    /// Unique identifier for the user making the request.
    pub user_id: String,
    /// Session identifier for conversation tracking.
    pub session_id: String,
    /// Previous messages in the conversation.
    pub conversation_history: Vec<Message>,
    /// User's stored memory and preferences.
    pub user_memory: Option<UserMemory>,
}

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// The role of the message sender.
    pub role: MessageRole,
    /// The message content.
    pub content: String,
    /// When the message was sent.
    pub timestamp: DateTime<Utc>,
}

/// Role of a message sender in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// System instructions to the model.
    System,
    /// Message from the user.
    User,
    /// Response from the assistant/agent.
    Assistant,
}

// ============= Memory Types =============

/// User memory containing preferences and learned facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMemory {
    /// The user's unique identifier.
    pub user_id: String,
    /// List of user preferences.
    pub preferences: Vec<Preference>,
    /// List of facts learned about the user.
    pub facts: Vec<MemoryFact>,
}

/// A user preference entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preference {
    /// Category of the preference (e.g., "communication", "output").
    pub category: String,
    /// Key identifying the specific preference.
    pub key: String,
    /// The preference value.
    pub value: String,
    /// Confidence score (0.0 to 1.0) for this preference.
    pub confidence: f32,
}

/// A fact learned about a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    /// Unique identifier for this fact.
    pub id: String,
    /// The user this fact belongs to.
    pub user_id: String,
    /// Category of the fact (e.g., "personal", "work").
    pub category: String,
    /// Key identifying the specific fact.
    pub fact_key: String,
    /// The fact value.
    pub fact_value: String,
    /// Confidence score (0.0 to 1.0) for this fact.
    pub confidence: f32,
    /// When this fact was first recorded.
    pub created_at: DateTime<Utc>,
    /// When this fact was last updated.
    pub updated_at: DateTime<Utc>,
}

// ============= Tool Types =============

/// Definition of a tool that can be called by an LLM.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolDefinition {
    /// Unique name of the tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema defining the tool's parameters.
    pub parameters: serde_json::Value,
}

/// A request to call a tool.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCall {
    /// Unique identifier for this tool call.
    pub id: String,
    /// Name of the tool to call.
    pub name: String,
    /// Arguments to pass to the tool.
    pub arguments: serde_json::Value,
}

/// Result from executing a tool.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolResult {
    /// ID of the tool call this result corresponds to.
    pub tool_call_id: String,
    /// The result data from the tool execution.
    pub result: serde_json::Value,
}

// ============= RAG Types =============

/// A document in the RAG knowledge base.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// Unique identifier for the document.
    pub id: String,
    /// The document's text content.
    pub content: String,
    /// Metadata about the document.
    pub metadata: DocumentMetadata,
    /// Optional embedding vector for semantic search.
    pub embedding: Option<Vec<f32>>,
}

/// Metadata associated with a document.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct DocumentMetadata {
    /// Title of the document.
    #[serde(default)]
    pub title: String,
    /// Source of the document (e.g., URL, file path).
    #[serde(default)]
    pub source: String,
    /// When the document was created or ingested.
    #[serde(default = "default_datetime")]
    pub created_at: DateTime<Utc>,
    /// Tags for categorization and filtering.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Query parameters for semantic search.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// The search query text.
    pub query: String,
    /// Maximum number of results to return.
    pub limit: usize,
    /// Minimum similarity threshold (0.0 to 1.0).
    pub threshold: f32,
    /// Optional filters to apply to results.
    pub filters: Option<Vec<SearchFilter>>,
}

/// A filter to apply during search.
#[derive(Debug, Clone)]
pub struct SearchFilter {
    /// Field name to filter on.
    pub field: String,
    /// Value to filter by.
    pub value: String,
}

/// A single search result with relevance score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// The matching document.
    pub document: Document,
    /// Similarity score (0.0 to 1.0).
    pub score: f32,
}

// ============= Authentication Types =============

/// Request payload for user login.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// User's email address.
    pub email: String,
    /// User's password.
    pub password: String,
}

/// Request payload for user registration.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegisterRequest {
    /// Email address for the new account.
    pub email: String,
    /// Password for the new account.
    pub password: String,
    /// Display name for the user.
    pub name: String,
}

/// Response containing authentication tokens.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TokenResponse {
    /// JWT access token for API authentication.
    pub access_token: String,
    /// Refresh token for obtaining new access tokens.
    pub refresh_token: String,
    /// Time in seconds until the access token expires.
    pub expires_in: i64,
}

/// JWT claims embedded in access tokens.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    /// Subject (user ID).
    pub sub: String,
    /// User's email address.
    pub email: String,
    /// Expiration time (Unix timestamp).
    pub exp: usize,
    /// Issued at time (Unix timestamp).
    pub iat: usize,
}

// ============= Error Types =============

/// Application-wide error type.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Database operation failed.
    #[error("Database error: {0}")]
    Database(String),

    /// LLM operation failed.
    #[error("LLM error: {0}")]
    LLM(String),

    /// Authentication or authorization failed.
    #[error("Authentication error: {0}")]
    Auth(String),

    /// Requested resource was not found.
    #[error("Not found: {0}")]
    NotFound(String),

    /// Input validation failed.
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// Configuration error.
    #[error("Configuration error: {0}")]
    Configuration(String),

    /// External service call failed.
    #[error("External service error: {0}")]
    External(String),

    /// Internal server error.
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
            AppError::Configuration(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::External(msg) => (axum::http::StatusCode::BAD_GATEWAY, msg),
            AppError::Internal(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg),
        };

        let body = serde_json::json!({
            "error": message
        });

        (status, axum::Json(body)).into_response()
    }
}

/// A specialized Result type for A.R.E.S operations.
pub type Result<T> = std::result::Result<T, AppError>;
