//! Ollama LLM client implementation
//!
//! This module provides integration with Ollama for local LLM inference.
//! Supports chat, generation, streaming, and comprehensive tool calling.
//!
//! # Features
//!
//! Enable with the `ollama` feature flag.
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::llm::{LLMClient, Provider};
//!
//! let provider = Provider::Ollama {
//!     base_url: "http://localhost:11434".to_string(),
//!     model: "ministral-3:3b".to_string(),
//! };
//! let client = provider.create_client().await?;
//! let response = client.generate("Hello!").await?;
//! ```

use crate::llm::client::{LLMClient, LLMResponse, ModelParams};
use crate::tools::registry::ToolRegistry;
use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use async_stream::stream;
use async_trait::async_trait;
use futures::{future::join_all, Stream, StreamExt};
use ollama_rs::{
    generation::chat::{request::ChatMessageRequest, ChatMessage},
    generation::tools::{ToolCall as OllamaToolCall, ToolFunctionInfo, ToolInfo, ToolType},
    models::ModelOptions,
    Ollama,
};
use schemars::Schema;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

/// Configuration for tool calling behavior
#[derive(Debug, Clone)]
pub struct ToolCallingConfig {
    /// Maximum number of tool calling iterations before stopping
    pub max_iterations: usize,
    /// Whether to include tool results in the final response
    pub include_tool_results: bool,
    /// Whether to enable parallel tool execution
    pub parallel_execution: bool,
    /// Timeout for individual tool execution in seconds
    pub tool_timeout_secs: u64,
}

impl Default for ToolCallingConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            include_tool_results: true,
            parallel_execution: false,
            tool_timeout_secs: 30,
        }
    }
}

/// Ollama LLM client implementation.
///
/// Connects to a local or remote Ollama server for inference.
pub struct OllamaClient {
    client: Ollama,
    model: String,
    tool_config: ToolCallingConfig,
    params: ModelParams,
}

impl OllamaClient {
    /// Creates a new OllamaClient with default tool configuration.
    pub async fn new(base_url: String, model: String) -> Result<Self> {
        Self::with_params(base_url, model, ModelParams::default()).await
    }

    /// Creates a new OllamaClient with model parameters.
    pub async fn with_params(base_url: String, model: String, params: ModelParams) -> Result<Self> {
        Self::with_config_and_params(base_url, model, ToolCallingConfig::default(), params).await
    }

    /// Creates a new OllamaClient with custom tool configuration.
    pub async fn with_config(
        base_url: String,
        model: String,
        tool_config: ToolCallingConfig,
    ) -> Result<Self> {
        Self::with_config_and_params(base_url, model, tool_config, ModelParams::default()).await
    }

    /// Creates a new OllamaClient with custom tool configuration and model parameters.
    pub async fn with_config_and_params(
        base_url: String,
        model: String,
        tool_config: ToolCallingConfig,
        params: ModelParams,
    ) -> Result<Self> {
        // ollama-rs' `Ollama::new(host, port)` parses `host` using reqwest's IntoUrl.
        // If `host` is something like "localhost" (no scheme), it panics with
        // `RelativeUrlWithoutBase`. To avoid server crashes, normalize user input
        // so we *always* pass an absolute URL like "http://localhost".
        //
        // Accept incoming configs like:
        // - http://localhost:11434
        // - https://example.com:11434
        // - localhost:11434
        // - localhost
        // - localhost:11434/api (path ignored)
        let trimmed = base_url.trim();
        if trimmed.is_empty() {
            return Err(AppError::Configuration(
                "OLLAMA_URL is empty/invalid; expected something like http://localhost:11434"
                    .to_string(),
            ));
        }

        // Strip scheme if present to get host[:port][/path...]
        let without_scheme = trimmed
            .strip_prefix("http://")
            .or_else(|| trimmed.strip_prefix("https://"))
            .unwrap_or(trimmed);

        // Drop any path/query fragments after the first '/'. E.g. "localhost:11434/api" → "localhost:11434"
        let host_port = without_scheme
            .split(&['/', '?', '#'][..])
            .next()
            .unwrap_or("localhost:11434");

        // Split host and port
        let (host, port) = if let Some(colon_idx) = host_port.rfind(':') {
            let h = &host_port[..colon_idx];
            let p_str = &host_port[colon_idx + 1..];
            let p = p_str.parse::<u16>().map_err(|_| {
                AppError::Configuration(format!(
                    "Invalid OLLAMA_URL port in '{}'; expected e.g. http://localhost:11434",
                    base_url
                ))
            })?;
            (h.to_string(), p)
        } else {
            (host_port.to_string(), 11434)
        };

        // ollama-rs Ollama::new expects an absolute URL; pass scheme+host
        let client = Ollama::new(format!("http://{}", host), port);

        Ok(Self {
            client,
            model,
            tool_config,
            params,
        })
    }

    /// Build ModelOptions from the stored params
    fn build_model_options(&self) -> ModelOptions {
        let mut options = ModelOptions::default();
        if let Some(temp) = self.params.temperature {
            options = options.temperature(temp);
        }
        if let Some(max_tokens) = self.params.max_tokens {
            options = options.num_predict(max_tokens as i32);
        }
        if let Some(top_p) = self.params.top_p {
            options = options.top_p(top_p);
        }
        // Note: ollama-rs uses repeat_penalty instead of separate frequency/presence penalties
        // We use presence_penalty as a fallback for repeat_penalty if set
        if let Some(pres_penalty) = self.params.presence_penalty {
            options = options.repeat_penalty(pres_penalty);
        }
        options
    }

    /// Get the tool calling configuration
    pub fn tool_config(&self) -> &ToolCallingConfig {
        &self.tool_config
    }

    /// Set the tool calling configuration
    pub fn set_tool_config(&mut self, config: ToolCallingConfig) {
        self.tool_config = config;
    }

    /// Convert our ToolDefinition to ollama-rs ToolInfo
    fn convert_tool_definition(tool: &ToolDefinition) -> ToolInfo {
        // Convert serde_json::Value to schemars Schema
        // ollama-rs expects a schemars Schema for parameters
        let schema: Schema =
            serde_json::from_value(tool.parameters.clone()).unwrap_or_else(|_| Schema::default());

        ToolInfo {
            tool_type: ToolType::Function,
            function: ToolFunctionInfo {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: schema,
            },
        }
    }

    /// Convert ollama-rs ToolCall to our ToolCall type
    fn convert_tool_call(call: &OllamaToolCall) -> ToolCall {
        ToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
        }
    }

    /// Execute a single tool call using the registry
    async fn execute_tool_call(
        registry: &ToolRegistry,
        tool_call: &ToolCall,
    ) -> Result<serde_json::Value> {
        registry
            .execute(&tool_call.name, tool_call.arguments.clone())
            .await
    }

    /// Format tool result for the model
    fn format_tool_result(tool_call: &ToolCall, result: &serde_json::Value) -> String {
        serde_json::to_string_pretty(&serde_json::json!({
            "tool_call_id": tool_call.id,
            "tool_name": tool_call.name,
            "result": result
        }))
        .unwrap_or_else(|_| format!("Result: {:?}", result))
    }
}

#[async_trait]
impl LLMClient for OllamaClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let messages = vec![ChatMessage::user(prompt.to_string())];

        let request = ChatMessageRequest::new(self.model.clone(), messages)
            .options(self.build_model_options());

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        // response.message is a ChatMessage, not Option<ChatMessage>
        Ok(response.message.content)
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let messages = vec![
            ChatMessage::system(system.to_string()),
            ChatMessage::user(prompt.to_string()),
        ];

        let request = ChatMessageRequest::new(self.model.clone(), messages)
            .options(self.build_model_options());

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        Ok(response.message.content)
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<String> {
        let chat_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|(role, content)| match role.as_str() {
                "system" => ChatMessage::system(content.clone()),
                "user" => ChatMessage::user(content.clone()),
                "assistant" => ChatMessage::assistant(content.clone()),
                _ => ChatMessage::user(content.clone()),
            })
            .collect();

        let request = ChatMessageRequest::new(self.model.clone(), chat_messages)
            .options(self.build_model_options());

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        Ok(response.message.content)
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        // Convert our tool definitions to ollama-rs format
        let ollama_tools: Vec<ToolInfo> = tools.iter().map(Self::convert_tool_definition).collect();

        let messages = vec![ChatMessage::user(prompt.to_string())];

        // Create request with tools and model options
        let request = ChatMessageRequest::new(self.model.clone(), messages)
            .tools(ollama_tools)
            .options(self.build_model_options());

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        // Extract content and tool calls from the message
        let content = response.message.content.clone();
        let tool_calls: Vec<ToolCall> = response
            .message
            .tool_calls
            .iter()
            .map(Self::convert_tool_call)
            .collect();

        // Determine finish reason based on whether tools were called
        let finish_reason = if tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        };

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason: finish_reason.to_string(),
        })
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let messages = vec![ChatMessage::user(prompt.to_string())];
        let request = ChatMessageRequest::new(self.model.clone(), messages)
            .options(self.build_model_options());

        let mut stream_response = self
            .client
            .send_chat_messages_stream(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama stream error: {}", e)))?;

        // Create an async stream that yields content chunks
        let output_stream = stream! {
            while let Some(chunk_result) = stream_response.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        // Each chunk has a message with content
                        let content = chunk.message.content;
                        if !content.is_empty() {
                            yield Ok(content);
                        }
                    }
                    Err(_) => {
                        yield Err(AppError::LLM("Stream chunk error".to_string()));
                        break;
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(output_stream)))
    }

    async fn stream_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let messages = vec![
            ChatMessage::system(system.to_string()),
            ChatMessage::user(prompt.to_string()),
        ];
        let request = ChatMessageRequest::new(self.model.clone(), messages)
            .options(self.build_model_options());

        let mut stream_response = self
            .client
            .send_chat_messages_stream(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama stream error: {}", e)))?;

        let output_stream = stream! {
            while let Some(chunk_result) = stream_response.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        let content = chunk.message.content;
                        if !content.is_empty() {
                            yield Ok(content);
                        }
                    }
                    Err(_) => {
                        yield Err(AppError::LLM("Stream chunk error".to_string()));
                        break;
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(output_stream)))
    }

    async fn stream_with_history(
        &self,
        messages: &[(String, String)],
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let chat_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|(role, content)| match role.as_str() {
                "system" => ChatMessage::system(content.clone()),
                "user" => ChatMessage::user(content.clone()),
                "assistant" => ChatMessage::assistant(content.clone()),
                _ => ChatMessage::user(content.clone()),
            })
            .collect();

        let request = ChatMessageRequest::new(self.model.clone(), chat_messages)
            .options(self.build_model_options());

        let mut stream_response = self
            .client
            .send_chat_messages_stream(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama stream error: {}", e)))?;

        let output_stream = stream! {
            while let Some(chunk_result) = stream_response.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        let content = chunk.message.content;
                        if !content.is_empty() {
                            yield Ok(content);
                        }
                    }
                    Err(_) => {
                        yield Err(AppError::LLM("Stream chunk error".to_string()));
                        break;
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(output_stream)))
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

/// Ollama Tool Coordinator - manages multi-turn tool calling conversations
pub struct OllamaToolCoordinator {
    client: Arc<OllamaClient>,
    registry: Arc<ToolRegistry>,
    config: ToolCallingConfig,
}

impl OllamaToolCoordinator {
    /// Creates a new coordinator with the default tool config from the client.
    pub fn new(client: Arc<OllamaClient>, registry: Arc<ToolRegistry>) -> Self {
        let config = client.tool_config().clone();
        Self {
            client,
            registry,
            config,
        }
    }

    /// Sets a custom tool calling configuration.
    pub fn with_config(mut self, config: ToolCallingConfig) -> Self {
        self.config = config;
        self
    }

    /// Execute a complete tool-calling conversation loop
    ///
    /// This method handles the full tool calling loop:
    /// 1. Send the initial prompt with available tools
    /// 2. If the model requests tool calls, execute them
    /// 3. Send tool results back to the model
    /// 4. Repeat until the model produces a final response or max iterations reached
    pub async fn execute(
        &self,
        system: Option<&str>,
        prompt: &str,
    ) -> Result<ToolCoordinatorResult> {
        let tools = self.registry.get_tool_definitions();
        let ollama_tools: Vec<ToolInfo> = tools
            .iter()
            .map(OllamaClient::convert_tool_definition)
            .collect();

        let mut messages: Vec<ChatMessage> = Vec::new();
        let mut all_tool_calls: Vec<ToolCallRecord> = Vec::new();

        // Add system message if provided
        if let Some(sys) = system {
            messages.push(ChatMessage::system(sys.to_string()));
        }

        // Add user message
        messages.push(ChatMessage::user(prompt.to_string()));

        for iteration in 0..self.config.max_iterations {
            let request = ChatMessageRequest::new(self.client.model.clone(), messages.clone())
                .tools(ollama_tools.clone());

            let response = self
                .client
                .client
                .send_chat_messages(request)
                .await
                .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

            let tool_calls: Vec<ToolCall> = response
                .message
                .tool_calls
                .iter()
                .map(OllamaClient::convert_tool_call)
                .collect();

            // If no tool calls, we're done
            if tool_calls.is_empty() {
                return Ok(ToolCoordinatorResult {
                    content: response.message.content,
                    tool_calls: all_tool_calls,
                    iterations: iteration + 1,
                    finish_reason: "stop".to_string(),
                });
            }

            // Add assistant message with tool calls to history
            messages.push(response.message.clone());

            // Execute each tool call and collect results (parallel if enabled)
            if self.config.parallel_execution {
                let timeout_secs = self.config.tool_timeout_secs;
                let registry = self.registry.clone();
                let results = join_all(tool_calls.into_iter().map(|tool_call| {
                    let registry = registry.clone();
                    async move {
                        let start_time = std::time::Instant::now();
                        let timed = timeout(
                            Duration::from_secs(timeout_secs),
                            OllamaClient::execute_tool_call(&registry, &tool_call),
                        )
                        .await;
                        let duration = start_time.elapsed();

                        let (result_value, success) = match timed {
                            Ok(Ok(value)) => (value, true),
                            Ok(Err(e)) => (serde_json::json!({"error": e.to_string()}), false),
                            Err(_) => (
                                serde_json::json!({"error": "tool execution timed out"}),
                                false,
                            ),
                        };

                        let record = ToolCallRecord {
                            id: tool_call.id.clone(),
                            name: tool_call.name.clone(),
                            arguments: tool_call.arguments.clone(),
                            result: result_value.clone(),
                            success,
                            duration_ms: duration.as_millis() as u64,
                        };

                        let result_str =
                            OllamaClient::format_tool_result(&tool_call, &result_value);
                        (record, ChatMessage::tool(result_str))
                    }
                }))
                .await;

                for (record, msg) in results {
                    all_tool_calls.push(record);
                    messages.push(msg);
                }
            } else {
                for tool_call in tool_calls {
                    let start_time = std::time::Instant::now();

                    let timed = timeout(
                        Duration::from_secs(self.config.tool_timeout_secs),
                        OllamaClient::execute_tool_call(&self.registry, &tool_call),
                    )
                    .await;

                    let duration = start_time.elapsed();

                    let (result_value, success) = match timed {
                        Ok(Ok(value)) => (value, true),
                        Ok(Err(e)) => (serde_json::json!({"error": e.to_string()}), false),
                        Err(_) => (
                            serde_json::json!({"error": "tool execution timed out"}),
                            false,
                        ),
                    };

                    all_tool_calls.push(ToolCallRecord {
                        id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments.clone(),
                        result: result_value.clone(),
                        success,
                        duration_ms: duration.as_millis() as u64,
                    });

                    let result_str = OllamaClient::format_tool_result(&tool_call, &result_value);
                    messages.push(ChatMessage::tool(result_str));
                }
            }
        }

        Err(AppError::LLM(format!(
            "Tool calling loop exceeded maximum iterations ({})",
            self.config.max_iterations
        )))
    }

    /// Execute with streaming response
    ///
    /// Returns a stream of partial responses during the final generation
    pub async fn execute_streaming(
        &self,
        system: Option<&str>,
        prompt: &str,
    ) -> Result<(
        Vec<ToolCallRecord>,
        Box<dyn Stream<Item = Result<String>> + Send + Unpin>,
    )> {
        let tools = self.registry.get_tool_definitions();
        let ollama_tools: Vec<ToolInfo> = tools
            .iter()
            .map(OllamaClient::convert_tool_definition)
            .collect();

        let mut messages: Vec<ChatMessage> = Vec::new();
        let mut all_tool_calls: Vec<ToolCallRecord> = Vec::new();

        if let Some(sys) = system {
            messages.push(ChatMessage::system(sys.to_string()));
        }
        messages.push(ChatMessage::user(prompt.to_string()));

        // Process tool calls (non-streaming)
        for _ in 0..self.config.max_iterations {
            let request = ChatMessageRequest::new(self.client.model.clone(), messages.clone())
                .tools(ollama_tools.clone());

            let response = self
                .client
                .client
                .send_chat_messages(request)
                .await
                .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

            let tool_calls: Vec<ToolCall> = response
                .message
                .tool_calls
                .iter()
                .map(OllamaClient::convert_tool_call)
                .collect();

            if tool_calls.is_empty() {
                // Start streaming the final response
                let request = ChatMessageRequest::new(self.client.model.clone(), messages.clone());

                let mut stream_response = self
                    .client
                    .client
                    .send_chat_messages_stream(request)
                    .await
                    .map_err(|e| AppError::LLM(format!("Ollama stream error: {}", e)))?;

                let output_stream = stream! {
                    while let Some(chunk_result) = stream_response.next().await {
                        match chunk_result {
                            Ok(chunk) => {
                                let content = chunk.message.content;
                                if !content.is_empty() {
                                    yield Ok(content);
                                }
                            }
                            Err(_) => {
                                yield Err(AppError::LLM("Stream chunk error".to_string()));
                                break;
                            }
                        }
                    }
                };

                return Ok((all_tool_calls, Box::new(Box::pin(output_stream))));
            }

            messages.push(response.message.clone());

            if self.config.parallel_execution {
                let timeout_secs = self.config.tool_timeout_secs;
                let registry = self.registry.clone();
                let results = join_all(tool_calls.into_iter().map(|tool_call| {
                    let registry = registry.clone();
                    async move {
                        let start_time = std::time::Instant::now();
                        let timed = timeout(
                            Duration::from_secs(timeout_secs),
                            OllamaClient::execute_tool_call(&registry, &tool_call),
                        )
                        .await;
                        let duration = start_time.elapsed();

                        let (result_value, success) = match timed {
                            Ok(Ok(value)) => (value, true),
                            Ok(Err(e)) => (serde_json::json!({"error": e.to_string()}), false),
                            Err(_) => (
                                serde_json::json!({"error": "tool execution timed out"}),
                                false,
                            ),
                        };

                        let record = ToolCallRecord {
                            id: tool_call.id.clone(),
                            name: tool_call.name.clone(),
                            arguments: tool_call.arguments.clone(),
                            result: result_value.clone(),
                            success,
                            duration_ms: duration.as_millis() as u64,
                        };

                        let result_str =
                            OllamaClient::format_tool_result(&tool_call, &result_value);
                        (record, ChatMessage::tool(result_str))
                    }
                }))
                .await;

                for (record, msg) in results {
                    all_tool_calls.push(record);
                    messages.push(msg);
                }
            } else {
                for tool_call in tool_calls {
                    let start_time = std::time::Instant::now();
                    let timed = timeout(
                        Duration::from_secs(self.config.tool_timeout_secs),
                        OllamaClient::execute_tool_call(&self.registry, &tool_call),
                    )
                    .await;
                    let duration = start_time.elapsed();

                    let (result_value, success) = match timed {
                        Ok(Ok(value)) => (value, true),
                        Ok(Err(e)) => (serde_json::json!({"error": e.to_string()}), false),
                        Err(_) => (
                            serde_json::json!({"error": "tool execution timed out"}),
                            false,
                        ),
                    };

                    all_tool_calls.push(ToolCallRecord {
                        id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        arguments: tool_call.arguments.clone(),
                        result: result_value.clone(),
                        success,
                        duration_ms: duration.as_millis() as u64,
                    });

                    let result_str = OllamaClient::format_tool_result(&tool_call, &result_value);
                    messages.push(ChatMessage::tool(result_str));
                }
            }
        }

        Err(AppError::LLM(format!(
            "Tool calling loop exceeded maximum iterations ({})",
            self.config.max_iterations
        )))
    }
}

/// Result from a tool coordinator execution
#[derive(Debug, Clone)]
pub struct ToolCoordinatorResult {
    /// The final text response from the model
    pub content: String,
    /// All tool calls that were made during the conversation
    pub tool_calls: Vec<ToolCallRecord>,
    /// Number of iterations the loop ran
    pub iterations: usize,
    /// Reason the conversation ended
    pub finish_reason: String,
}

/// Record of a single tool call execution
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    /// Unique identifier for this tool call
    pub id: String,
    /// Name of the tool that was called
    pub name: String,
    /// Arguments passed to the tool
    pub arguments: serde_json::Value,
    /// Result returned by the tool
    pub result: serde_json::Value,
    /// Whether the tool execution was successful
    pub success: bool,
    /// Time taken to execute the tool in milliseconds
    pub duration_ms: u64,
}

/// Extended Ollama client methods for convenience
impl OllamaClient {
    /// Execute a multi-turn conversation with tool calling (agent loop)
    ///
    /// This method handles the full tool calling loop:
    /// 1. Send the initial prompt with available tools
    /// 2. If the model requests tool calls, execute them
    /// 3. Send tool results back to the model
    /// 4. Repeat until the model produces a final response
    ///
    /// # Arguments
    /// * `system` - Optional system prompt
    /// * `prompt` - The user's prompt
    /// * `tools` - Available tool definitions
    /// * `tool_executor` - A function that executes tool calls and returns results
    /// * `max_iterations` - Maximum number of tool calling iterations (default: 10)
    pub async fn generate_with_tool_loop<F, Fut>(
        &self,
        system: Option<&str>,
        prompt: &str,
        tools: &[ToolDefinition],
        tool_executor: F,
        max_iterations: usize,
    ) -> Result<LLMResponse>
    where
        F: Fn(ToolCall) -> Fut,
        Fut: std::future::Future<Output = Result<serde_json::Value>>,
    {
        let ollama_tools: Vec<ToolInfo> = tools.iter().map(Self::convert_tool_definition).collect();

        let mut messages: Vec<ChatMessage> = Vec::new();

        // Add system message if provided
        if let Some(sys) = system {
            messages.push(ChatMessage::system(sys.to_string()));
        }

        // Add user message
        messages.push(ChatMessage::user(prompt.to_string()));

        for _ in 0..max_iterations {
            let request = ChatMessageRequest::new(self.model.clone(), messages.clone())
                .tools(ollama_tools.clone());

            let response = self
                .client
                .send_chat_messages(request)
                .await
                .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

            let tool_calls: Vec<ToolCall> = response
                .message
                .tool_calls
                .iter()
                .map(Self::convert_tool_call)
                .collect();

            // If no tool calls, we're done
            if tool_calls.is_empty() {
                return Ok(LLMResponse {
                    content: response.message.content,
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                });
            }

            // Add assistant message with tool calls to history
            messages.push(response.message.clone());

            // Execute each tool call and add results to messages
            for tool_call in tool_calls {
                let result = tool_executor(tool_call.clone()).await?;
                let result_str = serde_json::to_string(&result)
                    .unwrap_or_else(|_| "Error serializing result".to_string());

                // Add tool result message - ollama-rs ChatMessage::tool takes only content
                messages.push(ChatMessage::tool(result_str));
            }
        }

        Err(AppError::LLM(format!(
            "Tool calling loop exceeded maximum iterations ({})",
            max_iterations
        )))
    }

    /// Check if the Ollama server is available
    pub async fn health_check(&self) -> Result<bool> {
        // Try to list models - if this works, the server is up
        match self.client.list_local_models().await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// List available models on the Ollama server
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let models = self
            .client
            .list_local_models()
            .await
            .map_err(|e| AppError::LLM(format!("Failed to list models: {}", e)))?;

        // list_local_models returns Vec<LocalModel> directly
        Ok(models.into_iter().map(|m| m.name).collect())
    }

    /// Pull a model from the Ollama registry
    pub async fn pull_model(&self, model_name: &str) -> Result<()> {
        self.client
            .pull_model(model_name.to_string(), false)
            .await
            .map_err(|e| AppError::LLM(format!("Failed to pull model '{}': {}", model_name, e)))?;
        Ok(())
    }

    /// Get information about a specific model
    pub async fn model_info(&self, model_name: &str) -> Result<serde_json::Value> {
        let info = self
            .client
            .show_model_info(model_name.to_string())
            .await
            .map_err(|e| {
                AppError::LLM(format!(
                    "Failed to get model info for '{}': {}",
                    model_name, e
                ))
            })?;

        // Convert to JSON value
        Ok(serde_json::json!({
            "modelfile": info.modelfile,
            "parameters": info.parameters,
            "template": info.template,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_parsing_full() {
        let base_url = "http://localhost:11434";
        let url_parts: Vec<&str> = base_url.split("://").collect();
        assert_eq!(url_parts.len(), 2);
        assert_eq!(url_parts[0], "http");
        assert_eq!(url_parts[1], "localhost:11434");

        let host_port: Vec<&str> = url_parts[1].split(':').collect();
        assert_eq!(host_port[0], "localhost");
        assert_eq!(host_port[1], "11434");
    }

    #[test]
    fn test_url_parsing_no_port() {
        let base_url = "http://localhost";
        let url_parts: Vec<&str> = base_url.split("://").collect();
        let host_port: Vec<&str> = url_parts[1].split(':').collect();

        let host = host_port[0].to_string();
        let port = if host_port.len() == 2 {
            host_port[1].parse().unwrap_or(11434)
        } else {
            11434
        };

        assert_eq!(host, "localhost");
        assert_eq!(port, 11434);
    }

    #[test]
    fn test_url_parsing_custom_port() {
        let base_url = "http://192.168.1.100:8080";
        let url_parts: Vec<&str> = base_url.split("://").collect();
        let host_port: Vec<&str> = url_parts[1].split(':').collect();

        let host = host_port[0].to_string();
        let port: u16 = host_port[1].parse().unwrap_or(11434);

        assert_eq!(host, "192.168.1.100");
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_tool_definition_conversion() {
        let tool = ToolDefinition {
            name: "calculator".to_string(),
            description: "Performs basic math".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {"type": "string"},
                    "a": {"type": "number"},
                    "b": {"type": "number"}
                },
                "required": ["operation", "a", "b"]
            }),
        };

        let ollama_tool = OllamaClient::convert_tool_definition(&tool);
        assert_eq!(ollama_tool.function.name, "calculator");
        assert_eq!(ollama_tool.function.description, "Performs basic math");
    }

    #[test]
    fn test_tool_call_conversion() {
        let ollama_call = OllamaToolCall {
            function: ollama_rs::generation::tools::ToolCallFunction {
                name: "test_tool".to_string(),
                arguments: serde_json::json!({"arg1": "value1"}),
            },
        };

        let tool_call = OllamaClient::convert_tool_call(&ollama_call);
        assert_eq!(tool_call.name, "test_tool");
        assert_eq!(tool_call.arguments["arg1"], "value1");
        // ID should be a valid UUID
        assert!(!tool_call.id.is_empty());
    }

    #[test]
    fn test_tool_config_default() {
        let config = ToolCallingConfig::default();
        assert_eq!(config.max_iterations, 10);
        assert!(config.include_tool_results);
        assert!(!config.parallel_execution);
        assert_eq!(config.tool_timeout_secs, 30);
    }

    #[test]
    fn test_tool_call_record() {
        let record = ToolCallRecord {
            id: "test-id".to_string(),
            name: "test_tool".to_string(),
            arguments: serde_json::json!({"key": "value"}),
            result: serde_json::json!({"output": 42}),
            success: true,
            duration_ms: 100,
        };

        assert_eq!(record.id, "test-id");
        assert_eq!(record.name, "test_tool");
        assert!(record.success);
        assert_eq!(record.duration_ms, 100);
    }

    #[test]
    fn test_format_tool_result() {
        let tool_call = ToolCall {
            id: "call-123".to_string(),
            name: "calculator".to_string(),
            arguments: serde_json::json!({"a": 1, "b": 2}),
        };

        let result = serde_json::json!({"sum": 3});
        let formatted = OllamaClient::format_tool_result(&tool_call, &result);

        assert!(formatted.contains("call-123"));
        assert!(formatted.contains("calculator"));
        assert!(formatted.contains("sum"));
    }
}
