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

        // ollama-rs Ollama::new expects an absolute URL; pass scheme+host
        let client = Ollama::new(format!("http://{}", host), port);

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
    let parameters = info.get("parameters").and_then(|v| v.as_str()).unwrap_or("");
    let modelfile = info.get("modelfile").and_then(|v| v.as_str()).unwrap_or("");
    let template = info.get("template").and_then(|v| v.as_str()).unwrap_or("");
    let combined = format!("{parameters}
{modelfile}
{template}");
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
            let num_str = rest.trim().split_whitespace().next().unwrap_or(rest.trim());
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
    } else if lower.contains("json")
        || lower.contains("deserialize")
        || lower.contains("malformed")
    {
        AppError::LLM("Ollama stream malformed JSON".to_string())
    } else {
        AppError::LLM("Stream chunk error".to_string())
    }
}

fn map_ollama_error_message(msg: &str) -> AppError {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("429")
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::client::{LLMClient, ModelParams};
    use crate::coordinator::ConversationMessage;
    use futures::StreamExt;
    use wiremock::{matchers::*, Mock, MockServer, ResponseTemplate};

    async fn client_for(server: &MockServer) -> OllamaClient {
        OllamaClient::with_params(
            format!("http://127.0.0.1:{}", server.address().port()),
            "test-model".to_string(),
            ModelParams::default(),
        )
        .await
        .expect("ollama client")
    }

    fn chat_done_json(content: &str) -> String {
        serde_json::json!({
            "model": "test-model",
            "created_at": "2024-01-01T00:00:00Z",
            "message": { "role": "assistant", "content": content },
            "done": true
        })
        .to_string()
    }

    fn chat_done_with_usage_json(content: &str) -> String {
        serde_json::json!({
            "model": "test-model",
            "created_at": "2024-01-01T00:00:00Z",
            "message": { "role": "assistant", "content": content },
            "done": true,
            "total_duration": 1,
            "load_duration": 1,
            "prompt_eval_count": 12,
            "prompt_eval_duration": 1,
            "eval_count": 7,
            "eval_duration": 1
        })
        .to_string()
    }

    fn chat_stream_chunk_json(content: &str, done: bool) -> String {
        serde_json::json!({
            "model": "test-model",
            "created_at": "2024-01-01T00:00:00Z",
            "message": { "role": "assistant", "content": content },
            "done": done
        })
        .to_string()
    }

    fn ndjson_stream_body(lines: &[String]) -> String {
        let mut body = String::new();
        for line in lines {
            body.push_str(line);
            body.push('\n');
        }
        body
    }


    #[tokio::test]
    async fn test_with_params_standard_url() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await;
        assert!(client.is_ok());
        let client = client.unwrap();
        assert_eq!(client.model_name(), "test-model");
    }
    #[tokio::test]
    async fn test_with_params_https_url() {
        let client = OllamaClient::with_params(
            "https://example.com:11434".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await;
        assert!(client.is_ok());
    }
    #[tokio::test]
    async fn test_with_params_no_scheme() {
        let client = OllamaClient::with_params(
            "localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await;
        assert!(client.is_ok());
    }
    #[tokio::test]
    async fn test_with_params_no_scheme_no_port() {
        let client = OllamaClient::with_params(
            "localhost".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await;
        assert!(client.is_ok());
    }
    #[tokio::test]
    async fn test_with_params_path_stripped() {
        let client = OllamaClient::with_params(
            "http://localhost:11434/api".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await;
        assert!(client.is_ok());
    }
    #[tokio::test]
    async fn test_with_params_ip_with_port() {
        let client = OllamaClient::with_params(
            "192.168.1.100:8080".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await;
        assert!(client.is_ok());
    }
    #[tokio::test]
    async fn test_with_params_empty_string_errors() {
        let client = OllamaClient::with_params(
            "".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await;
        assert!(client.is_err());
    }
    #[tokio::test]
    async fn test_with_params_whitespace_string_errors() {
        let client = OllamaClient::with_params(
            "   ".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await;
        assert!(client.is_err());
    }
    #[tokio::test]
    async fn test_with_params_bad_port_errors() {
        let client = OllamaClient::with_params(
            "http://localhost:abc".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await;
        assert!(client.is_err());
    }
    #[tokio::test]
    async fn test_build_model_options_defaults() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await.unwrap();
        let json = serde_json::to_string(&client.build_model_options()).unwrap();
        let options: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(options.get("temperature").is_none() || options["temperature"].is_null());
        assert!(options.get("num_predict").is_none() || options["num_predict"].is_null());
    }
    #[tokio::test]
    async fn test_build_model_options_temperature_only() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams { temperature: Some(0.7), ..ModelParams::default() },
        ).await.unwrap();
        let json = serde_json::to_string(&client.build_model_options()).unwrap();
        let options: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(options["temperature"], 0.7);
    }
    #[tokio::test]
    async fn test_build_model_options_all_params() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams {
                temperature: Some(0.8),
                max_tokens: Some(100),
                top_p: Some(0.9),
                presence_penalty: Some(0.5),
                ..ModelParams::default()
            },
        ).await.unwrap();
        let json = serde_json::to_string(&client.build_model_options()).unwrap();
        let options: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(options["temperature"], 0.8);
        assert_eq!(options["num_predict"], 100);
        assert_eq!(options["top_p"], 0.9);
        assert_eq!(options["repeat_penalty"], 0.5);
    }
    #[tokio::test]
    async fn test_convert_conversation_message_system() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await.unwrap();
        let msg = ConversationMessage::system("You are helpful");
        let chat_msg = client.convert_conversation_message(&msg);
        assert_eq!(chat_msg.role, ollama_rs::generation::chat::MessageRole::System);
        assert_eq!(chat_msg.content, "You are helpful");
    }
    #[tokio::test]
    async fn test_convert_conversation_message_user() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await.unwrap();
        let msg = ConversationMessage::user("Hello");
        let chat_msg = client.convert_conversation_message(&msg);
        assert_eq!(chat_msg.role, ollama_rs::generation::chat::MessageRole::User);
        assert_eq!(chat_msg.content, "Hello");
    }
    #[tokio::test]
    async fn test_convert_conversation_message_assistant() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await.unwrap();
        let msg = ConversationMessage::assistant("Hi there", vec![]);
        let chat_msg = client.convert_conversation_message(&msg);
        assert_eq!(chat_msg.role, ollama_rs::generation::chat::MessageRole::Assistant);
        assert_eq!(chat_msg.content, "Hi there");
    }
    #[tokio::test]
    async fn test_convert_conversation_message_tool() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        ).await.unwrap();
        let msg = ConversationMessage::tool_result("call-123", &serde_json::json!({"result": "ok"}));
        let chat_msg = client.convert_conversation_message(&msg);
        assert_eq!(chat_msg.role, ollama_rs::generation::chat::MessageRole::Tool);
        assert!(chat_msg.content.contains("result"));
    }
    #[tokio::test]
    async fn test_model_name() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "my-custom-model".to_string(),
            ModelParams::default(),
        ).await.unwrap();
        assert_eq!(client.model_name(), "my-custom-model");
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
        assert!(!tool_call.id.is_empty());
    }

    #[test]
    fn test_tool_definition_invalid_schema_uses_default() {
        let tool = ToolDefinition {
            name: "broken".to_string(),
            description: "bad schema".to_string(),
            parameters: serde_json::json!("not-an-object"),
        };
        let ollama_tool = OllamaClient::convert_tool_definition(&tool);
        assert_eq!(ollama_tool.function.name, "broken");
        assert!(matches!(ollama_tool.tool_type, ToolType::Function));
    }

    #[test]
    fn test_ollama_provider_config_serde_roundtrip() {
        use ares_config::toml_config::ProviderConfig;

        let original = ProviderConfig::Ollama {
            base_url: "http://localhost:11434".to_string(),
            default_model: "llama3".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original.type_name(), decoded.type_name());
        match decoded {
            ProviderConfig::Ollama {
                base_url,
                default_model,
            } => {
                assert_eq!(base_url, "http://localhost:11434");
                assert_eq!(default_model, "llama3");
            }
            _ => panic!("expected Ollama variant"),
        }
    }

    #[test]
    fn test_from_config_model_override() {
        use ares_config::toml_config::ProviderConfig;
        use crate::client::Provider;

        let config = ProviderConfig::Ollama {
            base_url: "http://localhost:11434".to_string(),
            default_model: "default-model".to_string(),
        };
        let provider = Provider::from_config(&config, Some("override-model")).unwrap();
        match provider {
            Provider::Ollama { model, .. } => assert_eq!(model, "override-model"),
            _ => panic!("expected Ollama provider"),
        }
    }

    #[tokio::test]
    async fn test_with_params_trims_whitespace() {
        let client = OllamaClient::with_params(
            "  http://localhost:11434  ".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        )
        .await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_empty_url_configuration_error_message() {
        let result = OllamaClient::with_params(
            "".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        )
        .await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected Configuration error for empty URL"),
        };
        match err {
            AppError::Configuration(msg) => assert!(msg.contains("OLLAMA_URL")),
            _other => panic!("expected Configuration error"),
        }
    }

    #[tokio::test]
    async fn test_generate_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_json("mocked reply")),
            )
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let out = client.generate("hello").await.unwrap();
        assert_eq!(out, "mocked reply");
    }

    #[tokio::test]
    async fn test_generate_with_system_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_json("with system")),
            )
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let out = client
            .generate_with_system("You are helpful", "hi")
            .await
            .unwrap();
        assert_eq!(out, "with system");
    }

    #[tokio::test]
    async fn test_generate_with_history_includes_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_with_usage_json("history")),
            )
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let response = client
            .generate_with_history(&[("user".into(), "hello".into())])
            .await
            .unwrap();
        assert_eq!(response.content, "history");
        let usage = response.usage.expect("usage");
        assert_eq!(usage.prompt_tokens, 12);
        assert_eq!(usage.completion_tokens, 7);
    }

    #[tokio::test]
    async fn test_generate_with_tools_returns_tool_calls() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "model": "test-model",
            "created_at": "2024-01-01T00:00:00Z",
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": "calculator",
                        "arguments": {"a": 1}
                    }
                }]
            },
            "done": true
        })
        .to_string();

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let tools = vec![ToolDefinition {
            name: "calculator".to_string(),
            description: "calc".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let response = client.generate_with_tools("compute", &tools).await.unwrap();
        assert_eq!(response.finish_reason, "tool_calls");
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "calculator");
    }

    #[tokio::test]
    async fn test_generate_with_tools_and_history() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_done_json("ok")))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let messages = vec![ConversationMessage::user("run tool")];
        let response = client
            .generate_with_tools_and_history(&messages, &[])
            .await
            .unwrap();
        assert_eq!(response.content, "ok");
        assert_eq!(response.finish_reason, "stop");
    }

    #[tokio::test]
    async fn test_generate_maps_http_error_to_llm_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server blew up"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let err = client.generate("x").await.unwrap_err();
        match err {
            AppError::LLM(msg) => assert!(msg.contains("Ollama")),
            other => panic!("expected LLM error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_health_check_and_list_models() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [{
                    "name": "llama3",
                    "modified_at": "2024-01-01T00:00:00Z",
                    "size": 42
                }]
            })))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        assert!(client.health_check().await.unwrap());
        let models = client.list_models().await.unwrap();
        assert_eq!(models, vec!["llama3".to_string()]);
    }

    #[tokio::test]
    async fn test_model_info_serializes_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/show"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "modelfile": "FROM llama",
                "parameters": "temp 0.7",
                "template": "{{ .Prompt }}"
            })))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let info = client.model_info("llama3").await.unwrap();
        assert_eq!(info["modelfile"], "FROM llama");
        assert_eq!(info["parameters"], "temp 0.7");
    }

    #[tokio::test]
    async fn test_stream_yields_non_empty_chunks() {
        let chunk1 = serde_json::json!({
            "model": "test-model",
            "created_at": "2024-01-01T00:00:00Z",
            "message": { "role": "assistant", "content": "Hi" },
            "done": false
        });
        let chunk2 = serde_json::json!({
            "model": "test-model",
            "created_at": "2024-01-01T00:00:00Z",
            "message": { "role": "assistant", "content": "!" },
            "done": true
        });
        let body = format!("{chunk1}
{chunk2}
");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let mut stream = client.stream("hello").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "Hi");
        assert_eq!(stream.next().await.unwrap().unwrap(), "!");
    }
    #[tokio::test]
    async fn test_stream_maps_http_error_on_connect() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let err = match client.stream("hello").await {
            Err(e) => e,
            Ok(_) => panic!("expected stream setup to fail"),
        };
        match err {
            AppError::LLM(msg) => assert!(msg.contains("Ollama")),
            other => panic!("expected LLM error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stream_skips_empty_content_chunks() {
        let body = ndjson_stream_body(&[
            chat_stream_chunk_json("", false),
            chat_stream_chunk_json("", true),
        ]);

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let mut stream = client.stream("hello").await.unwrap();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_skips_malformed_json_line() {
        let valid = chat_stream_chunk_json("recovered", true);
        let body = format!("not-valid-json\n{valid}\n");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let mut stream = client.stream("hello").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "recovered");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_with_system_yields_chunks() {
        let body = ndjson_stream_body(&[
            chat_stream_chunk_json("sys-", false),
            chat_stream_chunk_json("reply", true),
        ]);

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let mut stream = client
            .stream_with_system("You are helpful", "hi")
            .await
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "sys-");
        assert_eq!(stream.next().await.unwrap().unwrap(), "reply");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_with_history_yields_chunks() {
        let body = ndjson_stream_body(&[
            chat_stream_chunk_json("hist", false),
            chat_stream_chunk_json("ory", true),
        ]);

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let mut stream = client
            .stream_with_history(&[("user".into(), "hello".into())])
            .await
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "hist");
        assert_eq!(stream.next().await.unwrap().unwrap(), "ory");
        assert!(stream.next().await.is_none());
    }




    fn sample_tool_definition(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "x": { "type": "number" } }
            }),
        }
    }

    fn chat_done_with_tools_json(content: &str, tool_names: &[&str]) -> String {
        let tool_calls: Vec<_> = tool_names
            .iter()
            .map(|name| {
                serde_json::json!({
                    "function": {
                        "name": name,
                        "arguments": { "x": 1 }
                    }
                })
            })
            .collect();
        serde_json::json!({
            "model": "test-model",
            "created_at": "2024-01-01T00:00:00Z",
            "message": {
                "role": "assistant",
                "content": content,
                "tool_calls": tool_calls
            },
            "done": true
        })
        .to_string()
    }

    async fn expect_llm_error<T, F>(fut: F, needle: &str)
    where
        F: std::future::Future<Output = Result<T>>,
    {
        match fut.await {
            Err(AppError::LLM(msg)) => assert!(
                msg.contains(needle),
                "expected LLM error containing {needle:?}, got {msg:?}"
            ),
            Err(other) => panic!("expected LLM error, got {other:?}"),
            Ok(_) => panic!("expected LLM error, got Ok"),
        }
    }

    #[allow(dead_code)]
    async fn expect_external_error<T, F>(fut: F, needle: &str)
    where
        F: std::future::Future<Output = Result<T>>,
    {
        match fut.await {
            Err(AppError::External(msg)) => assert!(
                msg.contains(needle),
                "expected External error containing {needle:?}, got {msg:?}"
            ),
            Err(other) => panic!("expected External error, got {other:?}"),
            Ok(_) => panic!("expected External error, got Ok"),
        }
    }

    #[tokio::test]
    async fn test_generate_with_tools_stop_finish_reason_without_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_json("plain answer")),
            )
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let tools = vec![sample_tool_definition("noop")];
        let response = client.generate_with_tools("hi", &tools).await.unwrap();
        assert_eq!(response.finish_reason, "stop");
        assert!(response.tool_calls.is_empty());
        assert_eq!(response.content, "plain answer");
    }

    #[tokio::test]
    async fn test_generate_with_tools_multiple_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                chat_done_with_tools_json("", &["alpha", "beta"]),
            ))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let response = client
            .generate_with_tools("run", &[sample_tool_definition("alpha")])
            .await
            .unwrap();
        assert_eq!(response.finish_reason, "tool_calls");
        assert_eq!(response.tool_calls.len(), 2);
        assert_eq!(response.tool_calls[0].name, "alpha");
        assert_eq!(response.tool_calls[1].name, "beta");
    }

    #[tokio::test]
    async fn test_generate_with_tools_request_body_includes_tool_schema() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_string_contains("weather_lookup"))
            .and(body_string_contains("\"type\":\"object\""))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_json("ok")),
            )
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let _ = client
            .generate_with_tools("forecast", &[sample_tool_definition("weather_lookup")])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_generate_with_tools_and_history_attaches_tools() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_string_contains("search_docs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_json("done")),
            )
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let messages = vec![
            ConversationMessage::system("be concise"),
            ConversationMessage::user("find docs"),
        ];
        let tools = vec![sample_tool_definition("search_docs")];
        let response = client
            .generate_with_tools_and_history(&messages, &tools)
            .await
            .unwrap();
        assert_eq!(response.content, "done");
        assert_eq!(response.finish_reason, "stop");
    }

    #[tokio::test]
    async fn test_generate_with_tools_and_history_tool_calls_finish_reason() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                chat_done_with_tools_json("", &["search_docs"]),
            ))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let messages = vec![ConversationMessage::user("search")];
        let tools = vec![sample_tool_definition("search_docs")];
        let response = client
            .generate_with_tools_and_history(&messages, &tools)
            .await
            .unwrap();
        assert_eq!(response.finish_reason, "tool_calls");
        assert_eq!(response.tool_calls[0].name, "search_docs");
    }

    #[tokio::test]
    async fn test_generate_with_history_system_and_assistant_roles() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_string_contains("\"role\":\"system\""))
            .and(body_string_contains("\"role\":\"assistant\""))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_json("roles ok")),
            )
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let response = client
            .generate_with_history(&[
                ("system".into(), "rules".into()),
                ("assistant".into(), "prior".into()),
                ("user".into(), "next".into()),
            ])
            .await
            .unwrap();
        assert_eq!(response.content, "roles ok");
        assert_eq!(response.finish_reason, "stop");
    }

    #[tokio::test]
    async fn test_generate_with_history_unknown_role_defaults_to_user() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_string_contains("\"role\":\"user\""))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_json("mapped")),
            )
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let response = client
            .generate_with_history(&[("tool".into(), "payload".into())])
            .await
            .unwrap();
        assert_eq!(response.content, "mapped");
    }

    #[tokio::test]
    async fn test_generate_with_history_no_usage_without_eval_counts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_json("no usage")),
            )
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let response = client
            .generate_with_history(&[("user".into(), "hi".into())])
            .await
            .unwrap();
        assert!(response.usage.is_none());
    }

    #[tokio::test]
    async fn test_generate_with_system_maps_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        expect_llm_error(
            client.generate_with_system("sys", "user"),
            "Ollama",
        )
        .await;
    }

    #[tokio::test]
    async fn test_generate_with_history_maps_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        expect_llm_error(
            client.generate_with_history(&[("user".into(), "x".into())]),
            "Ollama",
        )
        .await;
    }

    #[tokio::test]
    async fn test_generate_with_tools_maps_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        expect_llm_error(
            client.generate_with_tools("x", &[sample_tool_definition("t")]),
            "Ollama",
        )
        .await;
    }

    #[tokio::test]
    async fn test_generate_with_tools_and_history_maps_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        expect_llm_error(
            client.generate_with_tools_and_history(
                &[ConversationMessage::user("x")],
                &[sample_tool_definition("t")],
            ),
            "Ollama",
        )
        .await;
    }

    #[tokio::test]
    async fn test_health_check_false_when_tags_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(503).set_body_string("down"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        assert!(!client.health_check().await.unwrap());
    }

    #[tokio::test]
    async fn test_list_models_maps_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(500).set_body_string("fail"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        expect_llm_error(client.list_models(), "Ollama").await;
    }

    #[tokio::test]
    async fn test_model_info_maps_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/show"))
            .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        expect_llm_error(client.model_info("nope"), "Ollama").await;
    }

    #[tokio::test]
    async fn test_pull_model_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/pull"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "success"
            })))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        client.pull_model("llama3").await.unwrap();
    }

    #[tokio::test]
    async fn test_pull_model_maps_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/pull"))
            .respond_with(ResponseTemplate::new(500).set_body_string("pull failed"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        expect_llm_error(client.pull_model("llama3"), "Ollama").await;
    }

    #[tokio::test]
    async fn test_stream_with_history_system_and_assistant_roles() {
        let body = ndjson_stream_body(&[
            chat_stream_chunk_json("a", false),
            chat_stream_chunk_json("b", true),
        ]);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_string_contains("\"role\":\"system\""))
            .and(body_string_contains("\"role\":\"assistant\""))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let mut stream = client
            .stream_with_history(&[
                ("system".into(), "rules".into()),
                ("assistant".into(), "hi".into()),
                ("user".into(), "go".into()),
            ])
            .await
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "a");
        assert_eq!(stream.next().await.unwrap().unwrap(), "b");
    }

    #[tokio::test]
    async fn test_stream_with_history_unknown_role_defaults_to_user() {
        let body = ndjson_stream_body(&[chat_stream_chunk_json("only", true)]);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_string_contains("\"role\":\"user\""))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let mut stream = client
            .stream_with_history(&[("custom".into(), "payload".into())])
            .await
            .unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "only");
    }

    #[tokio::test]
    async fn test_build_model_options_max_tokens_only() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams {
                max_tokens: Some(512),
                ..ModelParams::default()
            },
        )
        .await
        .unwrap();
        let options: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&client.build_model_options()).unwrap())
                .unwrap();
        assert_eq!(options["num_predict"], 512);
        assert!(options.get("temperature").is_none() || options["temperature"].is_null());
    }

    #[tokio::test]
    async fn test_build_model_options_top_p_only() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams {
                top_p: Some(0.85),
                ..ModelParams::default()
            },
        )
        .await
        .unwrap();
        let options: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&client.build_model_options()).unwrap())
                .unwrap();
        assert_eq!(options["top_p"], 0.85);
    }

    #[tokio::test]
    async fn test_build_model_options_presence_penalty_maps_repeat_penalty() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams {
                presence_penalty: Some(1.1),
                ..ModelParams::default()
            },
        )
        .await
        .unwrap();
        let options: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&client.build_model_options()).unwrap())
                .unwrap();
        assert_eq!(options["repeat_penalty"], 1.1);
    }

    #[tokio::test]
    async fn test_build_model_options_frequency_penalty_not_mapped() {
        let client = OllamaClient::with_params(
            "http://localhost:11434".to_string(),
            "test-model".to_string(),
            ModelParams {
                frequency_penalty: Some(0.3),
                ..ModelParams::default()
            },
        )
        .await
        .unwrap();
        let options: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&client.build_model_options()).unwrap())
                .unwrap();
        assert!(options.get("repeat_penalty").is_none() || options["repeat_penalty"].is_null());
    }

    #[tokio::test]
    async fn test_with_params_query_fragment_stripped() {
        let client = OllamaClient::with_params(
            "http://localhost:11434?debug=1".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        )
        .await;
        assert!(client.is_ok());
    }

    #[test]
    fn test_tool_call_conversion_assigns_unique_ids() {
        let mk = |name: &str| OllamaToolCall {
            function: ollama_rs::generation::tools::ToolCallFunction {
                name: name.to_string(),
                arguments: serde_json::json!({}),
            },
        };
        let a = OllamaClient::convert_tool_call(&mk("a"));
        let b = OllamaClient::convert_tool_call(&mk("b"));
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn test_tool_definition_serializes_function_type() {
        let tool = OllamaClient::convert_tool_definition(&sample_tool_definition("metrics"));
        let value = serde_json::to_value(&tool).unwrap();
        assert_eq!(value["type"], "Function");
        assert_eq!(value["function"]["name"], "metrics");
        assert!(value["function"]["parameters"].is_object());
    }

    #[test]
    fn test_ollama_provider_config_json_tagged_deserialize() {
        use ares_config::toml_config::ProviderConfig;

        let json = r#"{"type":"ollama","base_url":"http://10.0.0.5:11434","default_model":"qwen"}"#;
        let decoded: ProviderConfig = serde_json::from_str(json).unwrap();
        match decoded {
            ProviderConfig::Ollama {
                base_url,
                default_model,
            } => {
                assert_eq!(base_url, "http://10.0.0.5:11434");
                assert_eq!(default_model, "qwen");
            }
            _ => panic!("expected Ollama variant"),
        }
    }

    #[test]
    fn test_from_config_without_override_uses_default_model() {
        use ares_config::toml_config::ProviderConfig;
        use crate::client::Provider;

        let config = ProviderConfig::Ollama {
            base_url: "http://localhost:11434".to_string(),
            default_model: "mistral".to_string(),
        };
        let provider = Provider::from_config(&config, None).unwrap();
        match provider {
            Provider::Ollama { model, base_url, .. } => {
                assert_eq!(model, "mistral");
                assert_eq!(base_url, "http://localhost:11434");
            }
            _ => panic!("expected Ollama provider"),
        }
    }

    #[test]
    fn test_provider_config_from_str_ollama_in_ollama_module() {
        use ares_config::toml_config::ProviderConfig;

        let config: ProviderConfig = "ollama".parse().unwrap();
        assert_eq!(config.type_name(), "ollama");
    }

    #[tokio::test]
    async fn test_bad_port_configuration_error_mentions_port() {
        let result = OllamaClient::with_params(
            "http://localhost:notaport".to_string(),
            "test-model".to_string(),
            ModelParams::default(),
        )
        .await;
        match result {
            Err(AppError::Configuration(msg)) => assert!(msg.contains("port")),
            Ok(_) => panic!("expected invalid port error"),
            _other => panic!("expected Configuration error"),
        }
    }


    // --- R48: error mapping, capabilities, streaming edge cases ---

    #[test]
    fn test_parse_retry_after_seconds() {
        assert_eq!(parse_retry_after("120"), Some(120));
        assert_eq!(parse_retry_after("  30  "), Some(30));
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("not-a-number"), None);
    }

    #[test]
    fn test_map_ollama_http_status_rate_limited_with_retry_after() {
        let err = map_ollama_http_status(429, "too many requests", Some(60));
        match err {
            AppError::RateLimited(msg) => {
                assert!(msg.contains("too many requests"));
                assert!(msg.contains("retry after 60s"));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn test_map_ollama_http_status_maps_auth_and_not_found() {
        assert!(matches!(
            map_ollama_http_status(401, "unauthorized", None),
            AppError::Auth(_)
        ));
        assert!(matches!(
            map_ollama_http_status(404, "missing model", None),
            AppError::NotFound(_)
        ));
    }

    #[test]
    fn test_map_ollama_http_status_maps_server_errors_to_external() {
        assert!(matches!(
            map_ollama_http_status(500, "internal", None),
            AppError::External(_)
        ));
    }

    #[test]
    fn test_map_ollama_error_message_detects_rate_limit_text() {
        assert!(matches!(
            map_ollama_http_status(400, "rate limit exceeded", None),
            AppError::RateLimited(_)
        ));
    }

    #[test]
    fn test_classify_stream_sse_failure_variants() {
        assert!(matches!(
            classify_stream_sse_failure("read timeout"),
            AppError::LLM(msg) if msg.contains("timeout")
        ));
        assert!(matches!(
            classify_stream_sse_failure("connection reset by peer"),
            AppError::LLM(msg) if msg.contains("disconnected")
        ));
        assert!(matches!(
            classify_stream_sse_failure("malformed JSON payload"),
            AppError::LLM(msg) if msg.contains("malformed JSON")
        ));
    }

    #[test]
    fn test_parse_model_capabilities_embeddings_and_json_mode() {
        let info = serde_json::json!({
            "modelfile": "FROM nomic-embed-text\nPARAMETER num_ctx 8192",
            "parameters": "format json\nnum_ctx 16384",
            "template": "{{ .Prompt }}",
            "capabilities": ["embedding"]
        });
        let caps = parse_model_capabilities_from_show(&info);
        assert!(caps.supports_embeddings);
        assert!(caps.supports_json_mode);
        assert_eq!(caps.context_window, Some(16384));
    }

    #[test]
    fn test_parse_model_capabilities_without_embeddings() {
        let info = serde_json::json!({
            "modelfile": "FROM llama3",
            "parameters": "num_ctx 4096",
            "template": ""
        });
        let caps = parse_model_capabilities_from_show(&info);
        assert!(!caps.supports_embeddings);
        assert!(!caps.supports_json_mode);
        assert_eq!(caps.context_window, Some(4096));
    }

    #[test]
    fn test_coerce_tool_argument_types_nested_and_enum() {
        let raw = serde_json::json!({
            "enabled": "true",
            "count": "42",
            "mode": "fast",
            "nested": { "ratio": "0.5", "flags": ["false", "1"] }
        });
        let coerced = coerce_tool_argument_types(&raw);
        assert_eq!(coerced["enabled"], serde_json::json!(true));
        assert_eq!(coerced["count"], serde_json::json!(42));
        assert_eq!(coerced["mode"], serde_json::json!("fast"));
        assert_eq!(coerced["nested"]["ratio"], serde_json::json!(0.5));
        assert_eq!(coerced["nested"]["flags"][0], serde_json::json!(false));
        assert_eq!(coerced["nested"]["flags"][1], serde_json::json!(1));
    }

    #[test]
    fn test_resolve_finish_reason_truncated_when_not_done() {
        assert_eq!(resolve_finish_reason(false, false), "length");
        assert_eq!(resolve_finish_reason(false, true), "stop");
        assert_eq!(resolve_finish_reason(true, true), "tool_calls");
    }

    #[test]
    fn test_tool_call_conversion_nested_arguments_coerced() {
        let ollama_call = OllamaToolCall {
            function: ollama_rs::generation::tools::ToolCallFunction {
                name: "search".to_string(),
                arguments: serde_json::json!({
                    "limit": "10",
                    "opts": { "deep": "true" }
                }),
            },
        };
        let tool_call = OllamaClient::convert_tool_call(&ollama_call);
        assert_eq!(tool_call.arguments["limit"], serde_json::json!(10));
        assert_eq!(tool_call.arguments["opts"]["deep"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn test_generate_empty_content_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_done_json("")))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        assert_eq!(client.generate("hi").await.unwrap(), "");
    }

    #[tokio::test]
    async fn test_generate_unicode_content() {
        let server = MockServer::start().await;
        let content = "Hello 世界 🌍 — café";
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_json(content)),
            )
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        assert_eq!(client.generate("ping").await.unwrap(), content);
    }

    #[tokio::test]
    async fn test_generate_http_429_maps_to_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limit exceeded"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let err = client.generate("x").await.unwrap_err();
        assert!(matches!(err, AppError::RateLimited(_)));
    }

    #[tokio::test]
    async fn test_generate_http_404_maps_to_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(404).set_body_string("model not found"))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let err = client.generate("x").await.unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn test_generate_with_history_truncated_finish_reason() {
        let body = serde_json::json!({
            "model": "test-model",
            "created_at": "2024-01-01T00:00:00Z",
            "message": { "role": "assistant", "content": "partial" },
            "done": false
        })
        .to_string();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let response = client
            .generate_with_history(&[("user".into(), "go".into())])
            .await
            .unwrap();
        assert_eq!(response.content, "partial");
        assert_eq!(response.finish_reason, "length");
    }

    #[tokio::test]
    async fn test_stream_partial_chunks_multi_content() {
        let body = ndjson_stream_body(&[
            chat_stream_chunk_json("Hel", false),
            chat_stream_chunk_json("lo ", false),
            chat_stream_chunk_json("🌍", true),
        ]);

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let mut stream = client.stream("hi").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "Hel");
        assert_eq!(stream.next().await.unwrap().unwrap(), "lo ");
        assert_eq!(stream.next().await.unwrap().unwrap(), "🌍");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_stream_mixed_empty_and_content_chunks() {
        let body = ndjson_stream_body(&[
            chat_stream_chunk_json("", false),
            chat_stream_chunk_json("only", false),
            chat_stream_chunk_json("", true),
        ]);

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let mut stream = client.stream("hi").await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "only");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_model_capabilities_from_show_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/show"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "modelfile": "FROM nomic-embed-text",
                "parameters": "num_ctx 8192\nformat json",
                "template": "{{ .Prompt }}",
                "capabilities": ["embedding"]
            })))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let caps = client.model_capabilities("nomic-embed-text").await.unwrap();
        assert!(caps.supports_embeddings);
        assert!(caps.supports_json_mode);
        assert_eq!(caps.context_window, Some(8192));
    }

    #[tokio::test]
    async fn test_generate_with_tools_nested_arguments() {
        let body = serde_json::json!({
            "model": "test-model",
            "created_at": "2024-01-01T00:00:00Z",
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": "search",
                        "arguments": {
                            "limit": "5",
                            "filter": { "active": "true", "tier": "pro" }
                        }
                    }
                }]
            },
            "done": true
        })
        .to_string();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let client = client_for(&server).await;
        let response = client
            .generate_with_tools("q", &[sample_tool_definition("search")])
            .await
            .unwrap();
        assert_eq!(response.tool_calls[0].arguments["limit"], serde_json::json!(5));
        assert_eq!(
            response.tool_calls[0].arguments["filter"]["active"],
            serde_json::json!(true)
        );
        assert_eq!(
            response.tool_calls[0].arguments["filter"]["tier"],
            serde_json::json!("pro")
        );
    }


}
