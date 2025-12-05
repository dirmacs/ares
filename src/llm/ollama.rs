//! Ollama LLM client implementation
//!
//! This module provides integration with Ollama for local LLM inference.
//! Supports chat, generation, streaming, and tool calling.
//!
//! # Features
//!
//! Enable with the `ollama` feature flag (included in default features).
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::llm::{LLMClient, Provider};
//!
//! let provider = Provider::Ollama {
//!     base_url: "http://localhost:11434".to_string(),
//!     model: "llama3.2".to_string(),
//! };
//! let client = provider.create_client().await?;
//! let response = client.generate("Hello!").await?;
//! ```

use crate::llm::client::{LLMClient, LLMResponse};
use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use async_stream::stream;
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use ollama_rs::{
    generation::chat::{request::ChatMessageRequest, ChatMessage},
    generation::tools::{ToolCall as OllamaToolCall, ToolInfo},
    Ollama,
};
use serde_json::json;

pub struct OllamaClient {
    client: Ollama,
    model: String,
}

impl OllamaClient {
    pub async fn new(base_url: String, model: String) -> Result<Self> {
        let url_parts: Vec<&str> = base_url.split("://").collect();
        let (host, port) = if url_parts.len() == 2 {
            let host_port: Vec<&str> = url_parts[1].split(':').collect();
            let host = host_port[0].to_string();
            let port = if host_port.len() == 2 {
                host_port[1].parse().unwrap_or(11434)
            } else {
                11434
            };
            (host, port)
        } else {
            ("localhost".to_string(), 11434)
        };

        let client = Ollama::new(host, port);

        Ok(Self { client, model })
    }

    /// Convert our ToolDefinition to ollama-rs ToolInfo
    fn convert_tool_definition(tool: &ToolDefinition) -> ToolInfo {
        // Extract properties and required fields from parameters schema
        let properties = tool
            .parameters
            .get("properties")
            .cloned()
            .unwrap_or(json!({}));

        let required = tool
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        ToolInfo::from_schema(tool.name.clone(), tool.description.clone(), properties, required)
    }

    /// Convert ollama-rs ToolCall to our ToolCall type
    fn convert_tool_call(call: &OllamaToolCall) -> ToolCall {
        ToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            name: call.function.name.clone(),
            arguments: call
                .function
                .arguments
                .clone()
                .unwrap_or(serde_json::json!({})),
        }
    }
}

#[async_trait]
impl LLMClient for OllamaClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let messages = vec![ChatMessage::user(prompt.to_string())];

        let request = ChatMessageRequest::new(self.model.clone(), messages);

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        Ok(response.message.map(|m| m.content).unwrap_or_default())
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let messages = vec![
            ChatMessage::system(system.to_string()),
            ChatMessage::user(prompt.to_string()),
        ];

        let request = ChatMessageRequest::new(self.model.clone(), messages);

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        Ok(response.message.map(|m| m.content).unwrap_or_default())
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

        let request = ChatMessageRequest::new(self.model.clone(), chat_messages);

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        Ok(response.message.map(|m| m.content).unwrap_or_default())
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        // Convert our tool definitions to ollama-rs format
        let ollama_tools: Vec<ToolInfo> = tools.iter().map(Self::convert_tool_definition).collect();

        let messages = vec![ChatMessage::user(prompt.to_string())];

        // Create request with tools
        let request = ChatMessageRequest::new(self.model.clone(), messages).tools(ollama_tools);

        let response = self
            .client
            .send_chat_messages(request)
            .await
            .map_err(|e| AppError::LLM(format!("Ollama error: {}", e)))?;

        // Extract content and tool calls
        let (content, tool_calls) = match response.message {
            Some(msg) => {
                let calls: Vec<ToolCall> = msg.tool_calls.iter().map(Self::convert_tool_call).collect();
                (msg.content, calls)
            }
            None => (String::new(), vec![]),
        };

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
        let request = ChatMessageRequest::new(self.model.clone(), messages);

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
                        // Each chunk may have message content
                        if let Some(msg) = chunk.message {
                            let content = msg.content;
                            if !content.is_empty() {
                                yield Ok(content);
                            }
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
        assert_eq!(ollama_tool.name, "calculator");
    }
}
