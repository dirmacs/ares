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
    /// Optional Eruka workspace_id for per-user context isolation.
    /// When set, the Eruka context middleware queries this workspace instead of the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
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

// ============= Semantic Search Types =============

/// Request for semantic document search.
/// Available only when `ares-vector` feature is enabled.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SemanticSearchRequest {
    /// Collection to search (tenant-scoped).
    pub collection: String,
    /// The search query text to embed.
    pub query: String,
    /// Maximum results to return (default: 10, max: 100).
    #[serde(default = "default_search_limit")]
    pub limit: usize,
    /// Minimum similarity threshold (0.0 to 1.0, default: 0.0).
    #[serde(default = "default_search_threshold")]
    pub threshold: f32,
}

/// Single semantic search result.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SemanticSearchResult {
    /// Document ID.
    pub id: String,
    /// Matching text content.
    pub content: String,
    /// Similarity score (0.0 to 1.0, higher is better).
    pub similarity: f32,
    /// Document metadata.
    pub metadata: DocumentMetadata,
}

/// Response from semantic search.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SemanticSearchResponse {
    /// Search results.
    pub results: Vec<SemanticSearchResult>,
    /// Total number of results found.
    pub total: usize,
    /// Query processing time in milliseconds.
    pub duration_ms: u64,
}

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
///
/// This enum supports both built-in agent types and custom user-defined agents.
/// The `Custom` variant allows for extensibility without modifying this enum.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
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
    #[serde(rename = "hr")]
    HR,
    /// Custom user-defined agent type.
    /// The string contains the agent's unique identifier/name.
    #[serde(untagged)]
    Custom(String),
}

impl AgentType {
    /// Returns the agent type name as a string slice.
    pub fn as_str(&self) -> &str {
        match self {
            AgentType::Router => "router",
            AgentType::Orchestrator => "orchestrator",
            AgentType::Product => "product",
            AgentType::Invoice => "invoice",
            AgentType::Sales => "sales",
            AgentType::Finance => "finance",
            AgentType::HR => "hr",
            AgentType::Custom(name) => name,
        }
    }

    /// Creates an AgentType from a string, using built-in types when possible.
    pub fn from_string(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "router" => AgentType::Router,
            "orchestrator" => AgentType::Orchestrator,
            "product" => AgentType::Product,
            "invoice" => AgentType::Invoice,
            "sales" => AgentType::Sales,
            "finance" => AgentType::Finance,
            "hr" => AgentType::HR,
            _ => AgentType::Custom(s.to_string()),
        }
    }

    /// Returns true if this is a built-in agent type.
    pub fn is_builtin(&self) -> bool {
        !matches!(self, AgentType::Custom(_))
    }
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
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
    /// JWT ID — unique per token (present on refresh tokens).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub jti: String,
}

// ============= Error Types =============

/// Error codes for programmatic error handling.
/// These are stable identifiers that clients can use to handle specific error cases.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// Database operation failed
    DatabaseError,
    /// LLM/AI model operation failed
    LlmError,
    /// Authentication failed (invalid credentials)
    AuthenticationFailed,
    /// Authorization failed (valid credentials but insufficient permissions)
    AuthorizationFailed,
    /// Requested resource was not found
    NotFound,
    /// Input validation failed
    InvalidInput,
    /// Server configuration error
    ConfigurationError,
    /// External service (API, webhook, etc.) failed
    ExternalServiceError,
    /// Internal server error
    InternalError,
}

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

    /// Service temporarily unavailable (emergency stop, maintenance).
    #[error("Service unavailable: {0}")]
 Unavailable(String),
    /// RAG feature was disabled by configuration
    #[error("Feature disabled: {0}")]
    FeatureDisabled(String),

 /// Rate limit / quota exceeded.
 #[error("Rate limited: {0}")]
 RateLimited(String),
}

impl AppError {
    /// Get the error code for this error type.
    pub fn code(&self) -> ErrorCode {
        match self {
            AppError::Database(_) => ErrorCode::DatabaseError,
            AppError::LLM(_) => ErrorCode::LlmError,
            AppError::Auth(_) => ErrorCode::AuthenticationFailed,
            AppError::NotFound(_) => ErrorCode::NotFound,
            AppError::InvalidInput(_) => ErrorCode::InvalidInput,
            AppError::Configuration(_) => ErrorCode::ConfigurationError,
            AppError::External(_) => ErrorCode::ExternalServiceError,
            AppError::Internal(_) => ErrorCode::InternalError,
            AppError::Unavailable(_) => ErrorCode::InternalError,
AppError::RateLimited(_) => ErrorCode::InternalError,
AppError::FeatureDisabled(_) => ErrorCode::InternalError,
    }
    }

    /// Check if this is an internal error that should be logged.
    fn is_internal(&self) -> bool {
        matches!(
            self,
            AppError::Database(_)
                | AppError::LLM(_)
                | AppError::Configuration(_)
                | AppError::Internal(_)
        )
    }

    /// Check if this error is transient and retrying with a fallback provider
    /// may succeed (timeout, rate limit, 5xx upstream).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AppError::External(_) | AppError::Unavailable(_) | AppError::RateLimited(_)
        )
    }
}

// ============= Error Conversions =============

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Internal(format!("IO error: {}", err))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::InvalidInput(format!("JSON error: {}", err))
    }
}

impl axum::response::IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        // Log internal errors before returning
        if self.is_internal() {
            tracing::error!(error = %self, code = ?self.code(), "Internal error occurred");
        }

        let (status, message) = match &self {
            AppError::Database(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            AppError::LLM(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            AppError::Auth(msg) => (axum::http::StatusCode::UNAUTHORIZED, msg.clone()),
            AppError::NotFound(msg) => (axum::http::StatusCode::NOT_FOUND, msg.clone()),
            AppError::InvalidInput(msg) => (axum::http::StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Configuration(msg) => {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg.clone())
            }
            AppError::External(msg) => (axum::http::StatusCode::BAD_GATEWAY, msg.clone()),
            AppError::Internal(msg) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            AppError::Unavailable(msg) => (axum::http::StatusCode::SERVICE_UNAVAILABLE, msg.clone()),
AppError::RateLimited(msg) => (axum::http::StatusCode::TOO_MANY_REQUESTS, msg.clone()),
AppError::FeatureDisabled(msg) => (axum::http::StatusCode::BAD_REQUEST, msg.clone()),
    };

        let body = serde_json::json!({
            "error": message,
            "code": self.code()
        });

        (status, axum::Json(body)).into_response()
    }
}

/// A specialized Result type for A.R.E.S operations.
pub type Result<T> = std::result::Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn test_agent_type_display_all_builtins() {
        let cases = vec![
            (AgentType::Router, "router"),
            (AgentType::Orchestrator, "orchestrator"),
            (AgentType::Product, "product"),
            (AgentType::Invoice, "invoice"),
            (AgentType::Sales, "sales"),
            (AgentType::Finance, "finance"),
            (AgentType::HR, "hr"),
        ];
        for (agent, expected) in cases {
            assert_eq!(agent.to_string(), expected);
            assert_eq!(format!("{}", agent), expected);
        }
    }

    #[test]
    fn test_agent_type_custom_display() {
        let custom = AgentType::Custom("my-agent".into());
        assert_eq!(custom.to_string(), "my-agent");
        assert!(!custom.is_builtin());
    }

    #[test]
    fn test_agent_type_from_string_roundtrip() {
        for name in ["router", "finance", "hr"] {
            let agent = AgentType::from_string(name);
            assert_eq!(agent.as_str(), name);
        }
        let custom = AgentType::from_string("custom-bot");
        assert_eq!(custom.as_str(), "custom-bot");
    }

    #[test]
    fn test_message_role_serde_roundtrip() {
        let role = MessageRole::Assistant;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, "\"assistant\"");
        let parsed: MessageRole = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, MessageRole::Assistant));
    }

    #[test]
    fn test_chat_request_serde_roundtrip() {
        let req = ChatRequest {
            message: "hello".into(),
            agent_type: Some(AgentType::Router),
            context_id: Some("ctx-1".into()),
            workspace_id: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.message, "hello");
        assert_eq!(parsed.agent_type, Some(AgentType::Router));
    }

    #[test]
    fn test_source_serde_roundtrip() {
        let source = Source {
            title: "Doc".into(),
            url: Some("https://example.com".into()),
            relevance_score: 0.9,
        };
        let parsed: Source = serde_json::from_str(&serde_json::to_string(&source).unwrap()).unwrap();
        assert_eq!(parsed.title, "Doc");
        assert_eq!(parsed.relevance_score, 0.9);
    }

    #[test]
    fn test_document_metadata_default_datetime() {
        let json = r#"{"title":"t","source":"s"}"#;
        let meta: DocumentMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(meta.title, "t");
        assert!(meta.created_at <= Utc::now());
    }

    #[test]
    fn test_tool_call_serde_roundtrip() {
        let call = ToolCall {
            id: "c1".into(),
            name: "search".into(),
            arguments: serde_json::json!({"q": "ares"}),
        };
        let parsed: ToolCall = serde_json::from_str(&serde_json::to_string(&call).unwrap()).unwrap();
        assert_eq!(parsed.name, "search");
    }

    #[test]
    fn test_app_error_code_mapping() {
        assert!(matches!(AppError::Database("x".into()).code(), ErrorCode::DatabaseError));
        assert!(matches!(AppError::Auth("x".into()).code(), ErrorCode::AuthenticationFailed));
        assert!(matches!(AppError::NotFound("x".into()).code(), ErrorCode::NotFound));
        assert!(matches!(AppError::RateLimited("x".into()).code(), ErrorCode::InternalError));
    }

    #[test]
    fn test_app_error_from_io() {
        let err: AppError = std::io::Error::new(std::io::ErrorKind::NotFound, "missing").into();
        assert!(matches!(err, AppError::Internal(_)));
        assert!(err.to_string().contains("IO error"));
    }

    #[test]
    fn test_app_error_from_serde_json() {
        let bad = "{not json";
        let err: AppError = serde_json::from_str::<serde_json::Value>(bad).unwrap_err().into();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[test]
    fn test_search_filter_application() {
        let doc = Document {
            id: "1".into(),
            content: "body".into(),
            metadata: DocumentMetadata {
                title: "Guide".into(),
                source: "docs/rust".into(),
                tags: vec!["rust".into(), "rag".into()],
                ..Default::default()
            },
            embedding: None,
        };
        let filters = vec![
            SearchFilter { field: "tags".into(), value: "rust".into() },
            SearchFilter { field: "source".into(), value: "docs/rust".into() },
        ];
        let matches = filters.iter().all(|f| match f.field.as_str() {
            "tags" => doc.metadata.tags.iter().any(|t| t == &f.value),
            "source" => doc.metadata.source == f.value,
            _ => false,
        });
        assert!(matches);
    }

    #[test]
    fn test_rag_search_request_defaults() {
        let json = r#"{"collection":"c","query":"q"}"#;
        let req: RagSearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.limit, 10);
        assert!((req.threshold - 0.0).abs() < f32::EPSILON);
        assert!(!req.rerank);
    }

    #[test]
    fn test_chat_response_serde_roundtrip() {
        let resp = ChatResponse {
            response: "こんにちは 🌍".into(),
            agent: "router".into(),
            context_id: String::new(),
            sources: Some(vec![]),
        };
        let parsed: ChatResponse =
            serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(parsed.response, "こんにちは 🌍");
        assert_eq!(parsed.context_id, "");
        assert!(parsed.sources.as_ref().unwrap().is_empty());
    }

    #[test]
    fn test_research_request_serde_optional_fields() {
        let json = r#"{"query":"quantum computing"}"#;
        let req: ResearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.query, "quantum computing");
        assert!(req.depth.is_none());
        assert!(req.max_iterations.is_none());

        let full = ResearchRequest {
            query: String::new(),
            depth: Some(0),
            max_iterations: Some(u8::MAX),
        };
        let parsed: ResearchRequest =
            serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!(parsed.query, "");
        assert_eq!(parsed.depth, Some(0));
        assert_eq!(parsed.max_iterations, Some(u8::MAX));
    }

    #[test]
    fn test_research_response_serde_empty_sources() {
        let resp = ResearchResponse {
            findings: String::new(),
            sources: vec![],
            duration_ms: 0,
        };
        let parsed: ResearchResponse =
            serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert!(parsed.findings.is_empty());
        assert!(parsed.sources.is_empty());
        assert_eq!(parsed.duration_ms, 0);
    }

    #[test]
    fn test_rag_ingest_request_defaults_and_unicode() {
        let json = r#"{"collection":"docs","content":"café ☕"}"#;
        let req: RagIngestRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.content, "café ☕");
        assert!(req.title.is_none());
        assert!(req.source.is_none());
        assert!(req.tags.is_empty());
        assert!(req.chunking_strategy.is_none());
    }

    #[test]
    fn test_rag_ingest_response_serde_roundtrip() {
        let resp = RagIngestResponse {
            chunks_created: 0,
            document_ids: vec![],
            collection: "empty".into(),
        };
        let parsed: RagIngestResponse =
            serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(parsed.chunks_created, 0);
        assert!(parsed.document_ids.is_empty());
    }

    #[test]
    fn test_rag_search_response_serde_roundtrip() {
        let resp = RagSearchResponse {
            results: vec![RagSearchResult {
                id: "d1".into(),
                content: "match".into(),
                score: 1.0,
                metadata: DocumentMetadata::default(),
            }],
            total: 1,
            strategy: "hybrid".into(),
            reranked: false,
            duration_ms: u64::MAX,
        };
        let parsed: RagSearchResponse =
            serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(parsed.total, 1);
        assert_eq!(parsed.duration_ms, u64::MAX);
    }

    #[test]
    fn test_rag_delete_collection_serde_roundtrip() {
        let req = RagDeleteCollectionRequest {
            collection: "to-delete".into(),
        };
        let parsed: RagDeleteCollectionRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(parsed.collection, "to-delete");

        let resp = RagDeleteCollectionResponse {
            success: true,
            collection: "to-delete".into(),
            documents_deleted: 0,
        };
        let parsed_resp: RagDeleteCollectionResponse =
            serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert!(parsed_resp.success);
        assert_eq!(parsed_resp.documents_deleted, 0);
    }

    #[test]
    fn test_semantic_search_request_defaults() {
        let json = r#"{"collection":"c","query":"q"}"#;
        let req: SemanticSearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.limit, 10);
        assert!((req.threshold - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_semantic_search_response_serde_roundtrip() {
        let resp = SemanticSearchResponse {
            results: vec![],
            total: 0,
            duration_ms: 0,
        };
        let parsed: SemanticSearchResponse =
            serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert!(parsed.results.is_empty());
        assert_eq!(parsed.total, 0);
    }

    #[test]
    fn test_workflow_request_empty_context_default() {
        let json = r#"{"query":"run workflow"}"#;
        let req: WorkflowRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.query, "run workflow");
        assert!(req.context.is_empty());

        let with_ctx = WorkflowRequest {
            query: "q".into(),
            context: [("key".into(), serde_json::json!(null))]
                .into_iter()
                .collect(),
        };
        let parsed: WorkflowRequest =
            serde_json::from_str(&serde_json::to_string(&with_ctx).unwrap()).unwrap();
        assert!(parsed.context.contains_key("key"));
    }

    #[test]
    fn test_agent_type_serde_builtin_and_custom_unicode() {
        for (agent, expected) in [
            (AgentType::Router, "\"router\""),
            (AgentType::HR, "\"hr\""),
        ] {
            let json = serde_json::to_string(&agent).unwrap();
            assert_eq!(json, expected);
            let parsed: AgentType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, agent);
        }
        let custom = AgentType::Custom("代理-🤖".into());
        let json = serde_json::to_string(&custom).unwrap();
        let parsed: AgentType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_str(), "代理-🤖");
    }

    #[test]
    fn test_agent_type_partial_eq_and_clone() {
        let a = AgentType::Finance;
        let b = a.clone();
        assert_eq!(a, b);
        assert_ne!(a, AgentType::Sales);
        assert!(a.is_builtin());
    }

    #[test]
    fn test_message_role_all_variants_serde() {
        for (role, expected) in [
            (MessageRole::System, "\"system\""),
            (MessageRole::User, "\"user\""),
            (MessageRole::Assistant, "\"assistant\""),
        ] {
            let json = serde_json::to_string(&role).unwrap();
            assert_eq!(json, expected);
            let parsed: MessageRole = serde_json::from_str(&json).unwrap();
            assert_eq!(format!("{:?}", parsed), format!("{:?}", role));
        }
    }

    #[test]
    fn test_message_serde_roundtrip_unicode() {
        let msg = Message {
            role: MessageRole::User,
            content: "emoji 🚀 & unicode ñ".into(),
            timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        };
        let parsed: Message =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(parsed.content, "emoji 🚀 & unicode ñ");
        assert!(matches!(parsed.role, MessageRole::User));
    }

    #[test]
    fn test_user_memory_empty_collections_serde() {
        let mem = UserMemory {
            user_id: "u0".into(),
            preferences: vec![],
            facts: vec![],
        };
        let parsed: UserMemory =
            serde_json::from_str(&serde_json::to_string(&mem).unwrap()).unwrap();
        assert!(parsed.preferences.is_empty());
        assert!(parsed.facts.is_empty());
    }

    #[test]
    fn test_preference_and_memory_fact_boundary_confidence() {
        let pref = Preference {
            category: String::new(),
            key: "lang".into(),
            value: "rust".into(),
            confidence: 0.0,
        };
        let parsed: Preference =
            serde_json::from_str(&serde_json::to_string(&pref).unwrap()).unwrap();
        assert!((parsed.confidence - 0.0).abs() < f32::EPSILON);

        let now = Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0).unwrap();
        let fact = MemoryFact {
            id: "f1".into(),
            user_id: "u1".into(),
            category: "work".into(),
            fact_key: "role".into(),
            fact_value: "engineer".into(),
            confidence: 1.0,
            created_at: now,
            updated_at: now,
        };
        let parsed_fact: MemoryFact =
            serde_json::from_str(&serde_json::to_string(&fact).unwrap()).unwrap();
        assert!((parsed_fact.confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_tool_definition_and_result_serde_roundtrip() {
        let def = ToolDefinition {
            name: "calc".into(),
            description: String::new(),
            parameters: serde_json::json!({}),
        };
        let parsed_def: ToolDefinition =
            serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
        assert_eq!(parsed_def.name, "calc");
        assert!(parsed_def.description.is_empty());

        let result = ToolResult {
            tool_call_id: "c1".into(),
            result: serde_json::Value::Null,
        };
        let parsed_result: ToolResult =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        assert!(parsed_result.result.is_null());
    }

    #[test]
    fn test_document_serde_none_embedding_and_metadata_default() {
        let doc = Document {
            id: "doc-1".into(),
            content: String::new(),
            metadata: DocumentMetadata::default(),
            embedding: None,
        };
        let parsed: Document =
            serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert!(parsed.content.is_empty());
        assert!(parsed.embedding.is_none());
        assert!(parsed.metadata.title.is_empty());

        let default_meta = DocumentMetadata::default();
        assert!(default_meta.tags.is_empty());
        assert!(default_meta.source.is_empty());
    }

    #[test]
    fn test_search_query_and_result_clone_debug() {
        let query = SearchQuery {
            query: "find".into(),
            limit: 0,
            threshold: 1.0,
            filters: None,
        };
        let cloned = query.clone();
        assert_eq!(cloned.limit, 0);
        assert!(cloned.filters.is_none());
        assert!(format!("{:?}", cloned).contains("find"));

        let result = SearchResult {
            document: Document {
                id: "1".into(),
                content: "x".into(),
                metadata: DocumentMetadata::default(),
                embedding: Some(vec![]),
            },
            score: 0.0,
        };
        let cloned_result = result.clone();
        assert!((cloned_result.score - 0.0).abs() < f32::EPSILON);
        assert!(cloned_result.document.embedding.as_ref().unwrap().is_empty());
    }

    #[test]
    fn test_agent_context_clone_debug() {
        let ctx = AgentContext {
            user_id: "u1".into(),
            session_id: "s1".into(),
            conversation_history: vec![],
            user_memory: None,
        };
        let cloned = ctx.clone();
        assert_eq!(cloned.user_id, "u1");
        assert!(cloned.user_memory.is_none());
        assert!(format!("{:?}", cloned).contains("AgentContext"));
    }

    #[test]
    fn test_login_register_token_claims_serde_roundtrip() {
        let login = LoginRequest {
            email: "user@example.com".into(),
            password: String::new(),
        };
        let parsed_login: LoginRequest =
            serde_json::from_str(&serde_json::to_string(&login).unwrap()).unwrap();
        assert!(parsed_login.password.is_empty());

        let register = RegisterRequest {
            email: "new@example.com".into(),
            password: "secret".into(),
            name: "新規ユーザー".into(),
        };
        let parsed_register: RegisterRequest =
            serde_json::from_str(&serde_json::to_string(&register).unwrap()).unwrap();
        assert_eq!(parsed_register.name, "新規ユーザー");

        let token = TokenResponse {
            access_token: "access".into(),
            refresh_token: "refresh".into(),
            expires_in: 0,
        };
        let parsed_token: TokenResponse =
            serde_json::from_str(&serde_json::to_string(&token).unwrap()).unwrap();
        assert_eq!(parsed_token.expires_in, 0);

        let claims = Claims {
            sub: "user-1".into(),
            email: "user@example.com".into(),
            exp: usize::MAX,
            iat: 0,
            jti: String::new(),
        };
        let json = serde_json::to_string(&claims).unwrap();
        assert!(!json.contains("jti"));
        let parsed_claims: Claims = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed_claims.jti, "");
    }

    #[test]
    fn test_error_code_serialize_all_variants() {
        let codes = [
            (ErrorCode::DatabaseError, "DATABASE_ERROR"),
            (ErrorCode::LlmError, "LLM_ERROR"),
            (ErrorCode::AuthenticationFailed, "AUTHENTICATION_FAILED"),
            (ErrorCode::AuthorizationFailed, "AUTHORIZATION_FAILED"),
            (ErrorCode::NotFound, "NOT_FOUND"),
            (ErrorCode::InvalidInput, "INVALID_INPUT"),
            (ErrorCode::ConfigurationError, "CONFIGURATION_ERROR"),
            (ErrorCode::ExternalServiceError, "EXTERNAL_SERVICE_ERROR"),
            (ErrorCode::InternalError, "INTERNAL_ERROR"),
        ];
        for (code, expected) in codes {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", expected));
        }
    }

    #[test]
    fn test_app_error_remaining_code_mappings() {
        assert!(matches!(
            AppError::LLM("x".into()).code(),
            ErrorCode::LlmError
        ));
        assert!(matches!(
            AppError::Configuration("x".into()).code(),
            ErrorCode::ConfigurationError
        ));
        assert!(matches!(
            AppError::External("x".into()).code(),
            ErrorCode::ExternalServiceError
        ));
        assert!(matches!(
            AppError::Unavailable("x".into()).code(),
            ErrorCode::InternalError
        ));
        assert!(matches!(
            AppError::FeatureDisabled("x".into()).code(),
            ErrorCode::InternalError
        ));
        assert!(matches!(
            AppError::Internal("x".into()).code(),
            ErrorCode::InternalError
        ));
    }

    #[test]
    fn test_source_clone_and_boundary_scores() {
        let source = Source {
            title: "t".into(),
            url: None,
            relevance_score: 0.0,
        };
        let cloned = source.clone();
        assert!(cloned.url.is_none());
        assert!((cloned.relevance_score - 0.0).abs() < f32::EPSILON);

        let max = Source {
            title: "max".into(),
            url: Some("https://example.com?q=100%".into()),
            relevance_score: 1.0,
        };
        let parsed: Source =
            serde_json::from_str(&serde_json::to_string(&max).unwrap()).unwrap();
        assert!((parsed.relevance_score - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_chat_request_workspace_id_serde_roundtrip() {
        let req = ChatRequest {
            message: "ping".into(),
            agent_type: None,
            context_id: None,
            workspace_id: Some("ws-éruka-42".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("workspace_id"));
        let parsed: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.workspace_id.as_deref(), Some("ws-éruka-42"));
    }

    #[test]
    fn test_rag_search_result_serde_roundtrip() {
        let result = RagSearchResult {
            id: "chunk-1".into(),
            content: "snippet".into(),
            score: 0.75,
            metadata: DocumentMetadata {
                title: "Guide".into(),
                source: "docs/guide.md".into(),
                tags: vec!["rag".into()],
                ..Default::default()
            },
        };
        let parsed: RagSearchResult =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        assert_eq!(parsed.id, "chunk-1");
        assert!((parsed.score - 0.75).abs() < f32::EPSILON);
        assert_eq!(parsed.metadata.tags, vec!["rag"]);
    }

    #[test]
    fn test_semantic_search_result_serde_roundtrip() {
        let result = SemanticSearchResult {
            id: "doc-9".into(),
            content: "semantic hit".into(),
            similarity: 0.91,
            metadata: DocumentMetadata::default(),
        };
        let parsed: SemanticSearchResult =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        assert_eq!(parsed.content, "semantic hit");
        assert!((parsed.similarity - 0.91).abs() < f32::EPSILON);
    }

    #[test]
    fn test_app_error_into_response_status_codes() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        let cases = [
            (AppError::Auth("denied".into()), StatusCode::UNAUTHORIZED),
            (AppError::NotFound("gone".into()), StatusCode::NOT_FOUND),
            (AppError::InvalidInput("bad".into()), StatusCode::BAD_REQUEST),
            (AppError::External("upstream".into()), StatusCode::BAD_GATEWAY),
            (
                AppError::Unavailable("maintenance".into()),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (AppError::RateLimited("slow".into()), StatusCode::TOO_MANY_REQUESTS),
            (AppError::FeatureDisabled("off".into()), StatusCode::BAD_REQUEST),
            (
                AppError::Database("db".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.into_response().status(), expected);
        }
    }

    #[test]
    fn test_agent_type_from_string_is_case_insensitive() {
        assert_eq!(AgentType::from_string("ROUTER"), AgentType::Router);
        assert_eq!(AgentType::from_string("Hr"), AgentType::HR);
        assert_eq!(
            AgentType::from_string("MyCustom"),
            AgentType::Custom("MyCustom".into())
        );
    }
}
