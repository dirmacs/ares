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
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        // Extract token usage from final_data if available
        let usage = response
            .final_data
            .as_ref()
            .map(|data| TokenUsage::new(data.prompt_eval_count as u32, data.eval_count as u32));

        Ok(LLMResponse {
            content: response.message.content,
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
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
            other => panic!("expected Configuration error, got {other:?}"),
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

}