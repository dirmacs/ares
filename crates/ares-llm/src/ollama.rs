//! Ollama LLM client implementation
//!
//! This module provides integration with Ollama for local LLM inference.
//! Supports chat, generation, streaming, and tool calling.
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

use crate::client::{LLMClient, LLMResponse, ModelParams, TokenUsage};
use crate::coordinator::{ConversationMessage, MessageRole};
use ares_types::types::{AppError, Result, ToolCall, ToolDefinition};
use async_stream::stream;
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use ollama_rs::{
    generation::chat::{request::ChatMessageRequest, ChatMessage},
    generation::tools::{ToolCall as OllamaToolCall, ToolFunctionInfo, ToolInfo, ToolType},
    models::ModelOptions,
    Ollama,
};
use schemars::Schema;

/// Ollama LLM client implementation.
///
/// Connects to a local or remote Ollama server for inference.
pub struct OllamaClient {
    client: Ollama,
    model: String,
    params: ModelParams,
}

impl OllamaClient {
    /// Creates a new OllamaClient with default parameters.
    pub async fn new(base_url: String, model: String) -> Result<Self> {
        Self::with_params(base_url, model, ModelParams::default()).await
    }

    /// Creates a new OllamaClient with model parameters.
    pub async fn with_params(base_url: String, model: String, params: ModelParams) -> Result<Self> {
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

        // ollama-rs Ollama::builder().host(...) expects an absolute URL; pass scheme+host
        let client = Ollama::builder()
            .host(format!("http://{}", host))
            .port(port)
            .build();

        Ok(Self {
            client,
            model,
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
            arguments: coerce_tool_argument_types(&call.function.arguments),
        }
    }

    /// Map ollama-rs errors to application error variants.
    fn map_ollama_error(err: &ollama_rs::error::OllamaError) -> AppError {
        use ollama_rs::error::OllamaError;

        match err {
            OllamaError::InternalError(inner) => map_ollama_error_message(&inner.message),
            OllamaError::Other(msg) => map_ollama_error_message(msg),
            OllamaError::ReqwestError(e) => {
                if e.is_timeout() {
                    AppError::LLM("Ollama request timed out".to_string())
                } else if e.is_connect() {
                    AppError::LLM("Ollama connection failed".to_string())
                } else {
                    map_ollama_error_message(&e.to_string())
                }
            }
            OllamaError::JsonError(e) => AppError::LLM(format!("Ollama JSON error: {e}")),
            OllamaError::ToolCallError(e) => {
                AppError::InvalidInput(format!("Ollama tool error: {e}"))
            }
        }
    }

    /// Convert a ConversationMessage to Ollama's ChatMessage
    fn convert_conversation_message(&self, msg: &ConversationMessage) -> ChatMessage {
        match msg.role {
            MessageRole::System => ChatMessage::system(msg.content.clone()),
            MessageRole::User => ChatMessage::user(msg.content.clone()),
            MessageRole::Assistant => {
                // Assistant messages - content only (tool calls are handled by context)
                ChatMessage::assistant(msg.content.clone())
            }
            MessageRole::Tool => {
                // For tool result messages, use Ollama's native tool message type
                ChatMessage::tool(msg.content.clone())
            }
        }
    }
}

/// Capabilities inferred from an Ollama `/api/show` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaModelCapabilities {
    pub supports_embeddings: bool,
    pub context_window: Option<u32>,
    pub supports_json_mode: bool,
}

/// Map HTTP status codes and bodies from Ollama to `AppError`.
#[allow(dead_code)]
pub(crate) fn map_ollama_http_status(
    status: u16,
    body: &str,
    retry_after_secs: Option<u64>,
) -> AppError {
    let retry_hint = retry_after_secs
        .map(|secs| format!(" (retry after {secs}s)"))
        .unwrap_or_default();
    match status {
        429 => AppError::RateLimited(format!("{body}{retry_hint}")),
        401 | 403 => AppError::Auth(body.to_string()),
        404 => AppError::NotFound(body.to_string()),
        408 | 504 => AppError::LLM(format!("Ollama timeout: {body}")),
        500..=599 => AppError::External(format!("Ollama server error ({status}): {body}")),
        _ => map_ollama_error_message(body),
    }
}

/// Parse a `Retry-After` header value (seconds).
#[allow(dead_code)]
pub(crate) fn parse_retry_after(header: &str) -> Option<u64> {
    let trimmed = header.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<u64>().ok()
}

/// Parse model capabilities from `/api/show` JSON.
pub(crate) fn parse_model_capabilities_from_show(
    info: &serde_json::Value,
) -> OllamaModelCapabilities {
    let parameters = info
        .get("parameters")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let modelfile = info.get("modelfile").and_then(|v| v.as_str()).unwrap_or("");
    let template = info.get("template").and_then(|v| v.as_str()).unwrap_or("");
    let combined = format!(
        "{parameters}
{modelfile}
{template}"
    );
    let lower = combined.to_ascii_lowercase();

    let supports_embeddings = lower.contains("embedding")
        || lower.contains("embed")
        || info
            .get("capabilities")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter().any(|v| {
                    v.as_str()
                        .map(|s| {
                            let s = s.to_ascii_lowercase();
                            s.contains("embed")
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

    let supports_json_mode =
        lower.contains("format json") || (lower.contains("format") && lower.contains("json"));

    OllamaModelCapabilities {
        supports_embeddings,
        context_window: parse_num_ctx_parameter(&combined),
        supports_json_mode,
    }
}

fn parse_num_ctx_parameter(text: &str) -> Option<u32> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("num_ctx") {
            let num_str = rest.split_whitespace().next().unwrap_or(rest.trim());
            if let Ok(n) = num_str.parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

/// Coerce string-encoded scalars in tool arguments (bools, numbers, enums stay strings).
pub(crate) fn coerce_tool_argument_types(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), coerce_tool_argument_types(v));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(coerce_tool_argument_types).collect())
        }
        serde_json::Value::String(s) => {
            if s == "true" || s == "false" {
                return serde_json::Value::Bool(s == "true");
            }
            if let Ok(n) = s.parse::<i64>() {
                return serde_json::json!(n);
            }
            if let Ok(n) = s.parse::<f64>() {
                if s.contains('.') {
                    return serde_json::json!(n);
                }
            }
            serde_json::Value::String(s.clone())
        }
        other => other.clone(),
    }
}

/// Resolve LLM finish reason from tool calls and stream completion flag.
pub(crate) fn resolve_finish_reason(has_tool_calls: bool, done: bool) -> &'static str {
    if has_tool_calls {
        "tool_calls"
    } else if done {
        "stop"
    } else {
        "length"
    }
}

/// Classify streaming/SSE failure modes for user-facing errors.
pub(crate) fn classify_stream_sse_failure(kind: &str) -> AppError {
    let lower = kind.to_ascii_lowercase();
    if lower.contains("timeout") {
        AppError::LLM("Ollama stream timeout".to_string())
    } else if lower.contains("disconnect")
        || lower.contains("reset")
        || lower.contains("broken pipe")
    {
        AppError::LLM("Ollama stream disconnected".to_string())
    } else if lower.contains("json") || lower.contains("deserialize") || lower.contains("malformed")
    {
        AppError::LLM("Ollama stream malformed JSON".to_string())
    } else {
        AppError::LLM("Stream chunk error".to_string())
    }
}

fn map_ollama_error_message(msg: &str) -> AppError {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("rate limit") || lower.contains("too many requests") || lower.contains("429")
    {
        return AppError::RateLimited(msg.to_string());
    }
    if lower.contains("unauthorized")
        || lower.contains("401")
        || lower.contains("forbidden")
        || lower.contains("403")
    {
        return AppError::Auth(msg.to_string());
    }
    if lower.contains("not found") || lower.contains("404") {
        return AppError::NotFound(msg.to_string());
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return AppError::LLM(format!("Ollama timeout: {msg}"));
    }
    if lower.contains("connection reset")
        || lower.contains("disconnect")
        || lower.contains("broken pipe")
    {
        return AppError::LLM(format!("Ollama disconnected: {msg}"));
    }
    AppError::LLM(format!("Ollama error: {msg}"))
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
            .map_err(|e| Self::map_ollama_error(&e))?;

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
            .map_err(|e| Self::map_ollama_error(&e))?;

        Ok(response.message.content)
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<LLMResponse> {
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
            .map_err(|e| Self::map_ollama_error(&e))?;

        // Extract token usage from final_data if available
        let usage = response
            .final_data
            .as_ref()
            .map(|data| TokenUsage::new(data.prompt_eval_count as u32, data.eval_count as u32));

        Ok(LLMResponse {
            content: response.message.content,
            tool_calls: vec![],
            finish_reason: resolve_finish_reason(false, response.done).to_string(),
            usage,
        })
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
            .map_err(|e| Self::map_ollama_error(&e))?;

        // Extract content and tool calls from the message
        let content = response.message.content.clone();
        let tool_calls: Vec<ToolCall> = response
            .message
            .tool_calls
            .iter()
            .map(Self::convert_tool_call)
            .collect();

        // Extract token usage from final_data if available
        let usage = response
            .final_data
            .as_ref()
            .map(|data| TokenUsage::new(data.prompt_eval_count as u32, data.eval_count as u32));

        let finish_reason =
            resolve_finish_reason(!tool_calls.is_empty(), response.done).to_string();
        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason,
            usage,
        })
    }

    async fn generate_with_tools_and_history(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        // Convert our tool definitions to ollama-rs format
        let ollama_tools: Vec<ToolInfo> = tools.iter().map(Self::convert_tool_definition).collect();

        // Convert ConversationMessage to Ollama ChatMessage
        let chat_messages: Vec<ChatMessage> = messages
            .iter()
            .map(|msg| self.convert_conversation_message(msg))
            .collect();

        // Create request with tools and model options
        let mut request = ChatMessageRequest::new(self.model.clone(), chat_messages)
            .options(self.build_model_options());

        if !ollama_tools.is_empty() {
            request = request.tools(ollama_tools);
        }

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| Self::map_ollama_error(&e))?;

        // Extract content and tool calls from the message
        let content = response.message.content.clone();
        let tool_calls: Vec<ToolCall> = response
            .message
            .tool_calls
            .iter()
            .map(Self::convert_tool_call)
            .collect();

        // Extract token usage from final_data if available
        let usage = response
            .final_data
            .as_ref()
            .map(|data| TokenUsage::new(data.prompt_eval_count as u32, data.eval_count as u32));

        let finish_reason =
            resolve_finish_reason(!tool_calls.is_empty(), response.done).to_string();
        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason,
            usage,
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
            .map_err(|e| Self::map_ollama_error(&e))?;

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
                        yield Err(classify_stream_sse_failure("transport"));
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
            .map_err(|e| Self::map_ollama_error(&e))?;

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
                        yield Err(classify_stream_sse_failure("transport"));
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
            .map_err(|e| Self::map_ollama_error(&e))?;

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
                        yield Err(classify_stream_sse_failure("transport"));
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

/// Extended Ollama client methods for convenience
impl OllamaClient {
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
            .map_err(|e| Self::map_ollama_error(&e))?;

        // list_local_models returns Vec<LocalModel> directly
        Ok(models.into_iter().map(|m| m.name).collect())
    }

    /// Pull a model from the Ollama registry
    pub async fn pull_model(&self, model_name: &str) -> Result<()> {
        self.client
            .pull_model(model_name.to_string(), false)
            .await
            .map_err(|e| Self::map_ollama_error(&e))?;
        Ok(())
    }

    /// Infer model capabilities from `/api/show` metadata.
    pub async fn model_capabilities(&self, model_name: &str) -> Result<OllamaModelCapabilities> {
        let info = self.model_info(model_name).await?;
        Ok(parse_model_capabilities_from_show(&info))
    }

    /// Get information about a specific model
    pub async fn model_info(&self, model_name: &str) -> Result<serde_json::Value> {
        let info = self
            .client
            .show_model_info(model_name.to_string())
            .await
            .map_err(|e| Self::map_ollama_error(&e))?;

        // Convert to JSON value
        Ok(serde_json::json!({
            "modelfile": info.modelfile,
            "parameters": info.parameters,
            "template": info.template,
            "capabilities": info.capabilities,
        }))
    }
}
