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

use crate::llm::client::{LLMClient, LLMResponse, ModelParams, TokenUsage};
use crate::llm::coordinator::{ConversationMessage, MessageRole};
use crate::types::{AppError, Result, ToolCall, ToolDefinition};
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
            arguments: call.function.arguments.clone(),
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

        // Extract token usage from final_data if available
        let usage = response
            .final_data
            .as_ref()
            .map(|data| TokenUsage::new(data.prompt_eval_count as u32, data.eval_count as u32));

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason: finish_reason.to_string(),
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

        // Extract token usage from final_data if available
        let usage = response
            .final_data
            .as_ref()
            .map(|data| TokenUsage::new(data.prompt_eval_count as u32, data.eval_count as u32));

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason: finish_reason.to_string(),
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
}
