//! Generic Tool Coordinator for Multi-Turn Tool Calling
//!
//! This module provides a provider-agnostic `ToolCoordinator` that works with any
//! `LLMClient` implementation. It handles the complete tool calling loop:
//!
//! 1. Send prompt with available tools to the LLM
//! 2. If the model requests tool calls, execute them
//! 3. Send tool results back to the model  
//! 4. Repeat until completion or max iterations
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::llm::coordinator::{ToolCoordinator, ToolCallingConfig};
//! use ares::llm::Provider;
//! use ares::tools::ToolRegistry;
//! use std::sync::Arc;
//!
//! let client = Provider::from_env()?.create_client().await?;
//! let registry = Arc::new(ToolRegistry::new());
//! let coordinator = ToolCoordinator::new(client, registry, ToolCallingConfig::default());
//!
//! let result = coordinator.execute(
//!     Some("You are a helpful assistant."),
//!     "What's 2 + 2?"
//! ).await?;
//!
//! println!("Response: {}", result.content);
//! println!("Tool calls made: {}", result.tool_calls.len());
//! ```

use crate::llm::client::{LLMClient, TokenUsage};
use crate::tools::registry::ToolRegistry;
use crate::types::{Result, ToolCall};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Configuration for tool calling coordination behavior.
///
/// Controls how the coordinator handles multi-turn tool calling,
/// including iteration limits, parallelism, and timeout settings.
#[derive(Debug, Clone)]
pub struct ToolCallingConfig {
    /// Maximum number of LLM iterations (not tool calls) before stopping.
    /// Each iteration is one round-trip to the LLM.
    pub max_iterations: usize,

    /// Whether to execute multiple tool calls in parallel.
    /// When false, tools are executed sequentially.
    pub parallel_execution: bool,

    /// Timeout for individual tool execution.
    pub tool_timeout: Duration,

    /// Whether to include tool results in the final response context.
    pub include_tool_results: bool,

    /// Whether to stop on the first tool error, or continue with remaining tools.
    pub stop_on_error: bool,
}

impl Default for ToolCallingConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            parallel_execution: true,
            tool_timeout: Duration::from_secs(30),
            include_tool_results: true,
            stop_on_error: false,
        }
    }
}

/// Record of a single tool call execution.
///
/// Captures all details about a tool invocation including timing,
/// success status, and any errors that occurred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Unique identifier for this tool call (from the LLM).
    pub id: String,
    /// Name of the tool that was called.
    pub name: String,
    /// Arguments passed to the tool.
    pub arguments: serde_json::Value,
    /// Result returned by the tool (or error object).
    pub result: serde_json::Value,
    /// Whether the tool execution was successful.
    pub success: bool,
    /// Time taken to execute the tool in milliseconds.
    pub duration_ms: u64,
    /// Error message if the tool failed.
    pub error: Option<String>,
}

/// Reason why a tool coordination session ended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FinishReason {
    /// Model decided to stop (no more tool calls).
    Stop,
    /// Hit the maximum iterations limit.
    MaxIterations,
    /// An unrecoverable error occurred.
    Error(String),
    /// Model tried to call an unknown tool.
    UnknownTool(String),
}

impl std::fmt::Display for FinishReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FinishReason::Stop => write!(f, "stop"),
            FinishReason::MaxIterations => write!(f, "max_iterations"),
            FinishReason::Error(e) => write!(f, "error: {}", e),
            FinishReason::UnknownTool(t) => write!(f, "unknown_tool: {}", t),
        }
    }
}

/// A message in a tool-calling conversation.
///
/// Represents all message types that can appear in a multi-turn
/// conversation with tool calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    /// The role of the message sender.
    pub role: MessageRole,
    /// The text content of the message.
    pub content: String,
    /// Tool calls requested by the assistant (only for Assistant role).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Tool result content (only for Tool role).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Role of a message sender in a tool-calling conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// System instructions.
    System,
    /// User message.
    User,
    /// Assistant response.
    Assistant,
    /// Tool execution result.
    Tool,
}

impl ConversationMessage {
    /// Create a system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Create an assistant message with optional tool calls.
    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
        }
    }

    /// Create a tool result message.
    pub fn tool_result(tool_call_id: impl Into<String>, result: &serde_json::Value) -> Self {
        Self {
            role: MessageRole::Tool,
            content: serde_json::to_string(result).unwrap_or_else(|_| "{}".to_string()),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    /// Convert to the simple (role, content) format for LLMClient::generate_with_history.
    pub fn to_role_content(&self) -> (String, String) {
        let role = match self.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        (role.to_string(), self.content.clone())
    }
}

/// Result of a complete tool coordination session.
///
/// Contains all information about what happened during the multi-turn
/// conversation, including the final response, all tool calls made,
/// token usage, and message history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatorResult {
    /// Final text response from the model.
    pub content: String,

    /// All tool calls made during the session.
    pub tool_calls: Vec<ToolCallRecord>,

    /// Number of LLM iterations (round-trips) performed.
    pub iterations: usize,

    /// Why the session ended.
    pub finish_reason: FinishReason,

    /// Accumulated token usage across all iterations.
    pub total_usage: TokenUsage,

    /// Full message history (useful for debugging and training data).
    pub message_history: Vec<ConversationMessage>,
}

/// Generic tool coordinator that works with any LLMClient.
///
/// Manages multi-turn tool calling conversations by:
/// 1. Sending prompts with tool definitions to the LLM
/// 2. Parsing tool call requests from the response
/// 3. Executing tools and collecting results
/// 4. Sending results back to the LLM
/// 5. Repeating until the LLM produces a final response
///
/// # Type Parameters
///
/// The coordinator is generic over the LLMClient, but typically you'll use
/// it with `Box<dyn LLMClient>` for maximum flexibility.
pub struct ToolCoordinator {
    client: Box<dyn LLMClient>,
    registry: Arc<ToolRegistry>,
    config: ToolCallingConfig,
}

impl ToolCoordinator {
    /// Create a new ToolCoordinator with the given client, registry, and config.
    pub fn new(
        client: Box<dyn LLMClient>,
        registry: Arc<ToolRegistry>,
        config: ToolCallingConfig,
    ) -> Self {
        Self {
            client,
            registry,
            config,
        }
    }

    /// Create a new ToolCoordinator with default configuration.
    pub fn with_defaults(client: Box<dyn LLMClient>, registry: Arc<ToolRegistry>) -> Self {
        Self::new(client, registry, ToolCallingConfig::default())
    }

    /// Execute a complete tool-calling conversation loop.
    ///
    /// This method handles the full tool calling loop:
    /// 1. Send the initial prompt with available tools
    /// 2. If the model requests tool calls, execute them
    /// 3. Send tool results back to the model
    /// 4. Repeat until the model produces a final response or max iterations reached
    ///
    /// # Arguments
    ///
    /// * `system` - Optional system prompt
    /// * `prompt` - The user's prompt
    ///
    /// # Returns
    ///
    /// A `CoordinatorResult` containing the final response, all tool calls made,
    /// and execution metadata.
    pub async fn execute(&self, system: Option<&str>, prompt: &str) -> Result<CoordinatorResult> {
        let tools = self.registry.get_tool_definitions();
        let mut messages: Vec<ConversationMessage> = Vec::new();
        let mut all_tool_calls: Vec<ToolCallRecord> = Vec::new();
        let mut total_usage = TokenUsage::default();

        // Add system message if provided
        if let Some(sys) = system {
            messages.push(ConversationMessage::system(sys));
        }

        // Add user message
        messages.push(ConversationMessage::user(prompt));

        for iteration in 0..self.config.max_iterations {
            // Call LLM with tools
            let response = self
                .client
                .generate_with_tools_and_history(&messages, &tools)
                .await?;

            // Accumulate usage
            if let Some(usage) = &response.usage {
                total_usage = TokenUsage::new(
                    total_usage.prompt_tokens + usage.prompt_tokens,
                    total_usage.completion_tokens + usage.completion_tokens,
                );
            }

            // Add assistant message to history
            messages.push(ConversationMessage::assistant(
                &response.content,
                response.tool_calls.clone(),
            ));

            // Check if we're done (no tool calls)
            if response.tool_calls.is_empty() {
                return Ok(CoordinatorResult {
                    content: response.content,
                    tool_calls: all_tool_calls,
                    iterations: iteration + 1,
                    finish_reason: FinishReason::Stop,
                    total_usage,
                    message_history: messages,
                });
            }

            // Validate that all requested tools exist
            for tool_call in &response.tool_calls {
                if !self.registry.has_tool(&tool_call.name) {
                    return Ok(CoordinatorResult {
                        content: response.content,
                        tool_calls: all_tool_calls,
                        iterations: iteration + 1,
                        finish_reason: FinishReason::UnknownTool(tool_call.name.clone()),
                        total_usage,
                        message_history: messages,
                    });
                }
            }

            // Execute tool calls
            let tool_results = self.execute_tool_calls(&response.tool_calls).await?;

            // Record tool calls and add results to message history
            for record in tool_results {
                // Add tool result to messages
                messages.push(ConversationMessage::tool_result(&record.id, &record.result));
                all_tool_calls.push(record);
            }
        }

        // Hit max iterations
        Ok(CoordinatorResult {
            content: messages
                .last()
                .map(|m| m.content.clone())
                .unwrap_or_default(),
            tool_calls: all_tool_calls,
            iterations: self.config.max_iterations,
            finish_reason: FinishReason::MaxIterations,
            total_usage,
            message_history: messages,
        })
    }

    /// Execute tool calls, either in parallel or sequentially based on config.
    async fn execute_tool_calls(&self, calls: &[ToolCall]) -> Result<Vec<ToolCallRecord>> {
        if self.config.parallel_execution {
            self.execute_parallel(calls).await
        } else {
            self.execute_sequential(calls).await
        }
    }

    /// Execute tool calls in parallel.
    async fn execute_parallel(&self, calls: &[ToolCall]) -> Result<Vec<ToolCallRecord>> {
        let futures = calls.iter().map(|call| self.execute_single_tool(call));
        let results = join_all(futures).await;

        let mut records = Vec::with_capacity(results.len());
        for result in results {
            match result {
                Ok(record) => records.push(record),
                Err(e) if self.config.stop_on_error => return Err(e),
                Err(e) => {
                    // Create an error record for failed tools
                    records.push(ToolCallRecord {
                        id: "error".to_string(),
                        name: "unknown".to_string(),
                        arguments: serde_json::Value::Null,
                        result: serde_json::json!({"error": e.to_string()}),
                        success: false,
                        duration_ms: 0,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        Ok(records)
    }

    /// Execute tool calls sequentially.
    async fn execute_sequential(&self, calls: &[ToolCall]) -> Result<Vec<ToolCallRecord>> {
        let mut records = Vec::with_capacity(calls.len());
        for call in calls {
            match self.execute_single_tool(call).await {
                Ok(record) => records.push(record),
                Err(e) if self.config.stop_on_error => return Err(e),
                Err(e) => {
                    records.push(ToolCallRecord {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                        result: serde_json::json!({"error": e.to_string()}),
                        success: false,
                        duration_ms: 0,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        Ok(records)
    }

    /// Execute a single tool call with timeout.
    async fn execute_single_tool(&self, call: &ToolCall) -> Result<ToolCallRecord> {
        let start = Instant::now();

        let result = timeout(
            self.config.tool_timeout,
            self.registry.execute(&call.name, call.arguments.clone()),
        )
        .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(value)) => Ok(ToolCallRecord {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                result: value,
                success: true,
                duration_ms,
                error: None,
            }),
            Ok(Err(e)) => Ok(ToolCallRecord {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                result: serde_json::json!({"error": e.to_string()}),
                success: false,
                duration_ms,
                error: Some(e.to_string()),
            }),
            Err(_) => Ok(ToolCallRecord {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                result: serde_json::json!({"error": "Tool execution timed out"}),
                success: false,
                duration_ms,
                error: Some("Tool execution timed out".to_string()),
            }),
        }
    }

    /// Get a reference to the underlying LLM client.
    pub fn client(&self) -> &dyn LLMClient {
        self.client.as_ref()
    }

    /// Get a reference to the tool registry.
    pub fn registry(&self) -> &Arc<ToolRegistry> {
        &self.registry
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &ToolCallingConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_calling_config_default() {
        let config = ToolCallingConfig::default();
        assert_eq!(config.max_iterations, 10);
        assert!(config.parallel_execution);
        assert_eq!(config.tool_timeout, Duration::from_secs(30));
        assert!(config.include_tool_results);
        assert!(!config.stop_on_error);
    }

    #[test]
    fn test_conversation_message_system() {
        let msg = ConversationMessage::system("You are a helpful assistant.");
        assert_eq!(msg.role, MessageRole::System);
        assert_eq!(msg.content, "You are a helpful assistant.");
        assert!(msg.tool_calls.is_empty());
        assert!(msg.tool_call_id.is_none());
    }

    #[test]
    fn test_conversation_message_user() {
        let msg = ConversationMessage::user("Hello!");
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.content, "Hello!");
    }

    #[test]
    fn test_conversation_message_assistant_with_tool_calls() {
        let tool_calls = vec![ToolCall {
            id: "call_1".to_string(),
            name: "calculator".to_string(),
            arguments: serde_json::json!({"a": 1, "b": 2}),
        }];
        let msg = ConversationMessage::assistant("Let me calculate that.", tool_calls.clone());
        assert_eq!(msg.role, MessageRole::Assistant);
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].name, "calculator");
    }

    #[test]
    fn test_conversation_message_tool_result() {
        let result = serde_json::json!({"result": 42});
        let msg = ConversationMessage::tool_result("call_1", &result);
        assert_eq!(msg.role, MessageRole::Tool);
        assert_eq!(msg.tool_call_id, Some("call_1".to_string()));
        assert!(msg.content.contains("42"));
    }

    #[test]
    fn test_finish_reason_display() {
        assert_eq!(FinishReason::Stop.to_string(), "stop");
        assert_eq!(FinishReason::MaxIterations.to_string(), "max_iterations");
        assert_eq!(
            FinishReason::Error("test error".to_string()).to_string(),
            "error: test error"
        );
        assert_eq!(
            FinishReason::UnknownTool("unknown".to_string()).to_string(),
            "unknown_tool: unknown"
        );
    }

    #[test]
    fn test_tool_call_record_serialization() {
        let record = ToolCallRecord {
            id: "call_1".to_string(),
            name: "test_tool".to_string(),
            arguments: serde_json::json!({"input": "test"}),
            result: serde_json::json!({"output": "result"}),
            success: true,
            duration_ms: 100,
            error: None,
        };

        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("test_tool"));
        assert!(json.contains("\"success\":true"));
    }

    #[test]
    fn test_message_to_role_content() {
        let msg = ConversationMessage::user("Hello");
        let (role, content) = msg.to_role_content();
        assert_eq!(role, "user");
        assert_eq!(content, "Hello");

        let msg = ConversationMessage::system("System prompt");
        let (role, content) = msg.to_role_content();
        assert_eq!(role, "system");
        assert_eq!(content, "System prompt");
    }
}
