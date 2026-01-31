//! Anthropic Claude LLM client implementation
//!
//! This module provides integration with the Anthropic Claude API.
//!
//! # Features
//!
//! Enable with the `anthropic` feature flag.
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::llm::{LLMClient, Provider};
//!
//! let provider = Provider::Anthropic {
//!     api_key: "sk-ant-...".to_string(),
//!     model: "claude-3-5-sonnet-20241022".to_string(),
//!     params: ModelParams::default(),
//! };
//! let client = provider.create_client().await?;
//! let response = client.generate("Hello!").await?;
//! ```

use crate::llm::client::{LLMClient, LLMResponse, ModelParams, TokenUsage};
use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use async_trait::async_trait;
use claude_sdk::{
    ClaudeClient, ContentBlock, Message, MessagesRequest, StopReason, StreamEvent, Tool,
};
use futures::StreamExt;

/// Anthropic Claude client for API-based inference
pub struct AnthropicClient {
    client: ClaudeClient,
    model: String,
    params: ModelParams,
}

impl AnthropicClient {
    /// Create a new Anthropic client
    ///
    /// # Arguments
    ///
    /// * `api_key` - Anthropic API key
    /// * `model` - Model identifier (e.g., "claude-3-5-sonnet-20241022")
    pub fn new(api_key: String, model: String) -> Self {
        Self::with_params(api_key, model, ModelParams::default())
    }

    /// Create a new Anthropic client with model parameters
    ///
    /// # Arguments
    ///
    /// * `api_key` - Anthropic API key
    /// * `model` - Model identifier (e.g., "claude-3-5-sonnet-20241022")
    /// * `params` - Model inference parameters (temperature, max_tokens, etc.)
    pub fn with_params(api_key: String, model: String, params: ModelParams) -> Self {
        let client = ClaudeClient::anthropic(api_key);

        Self {
            client,
            model,
            params,
        }
    }

    /// Get the max tokens, defaulting to 1024 if not specified
    fn max_tokens(&self) -> u32 {
        self.params.max_tokens.unwrap_or(1024)
    }

    /// Convert a ToolDefinition to a Claude Tool
    fn convert_tool(tool: &ToolDefinition) -> Tool {
        Tool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.parameters.clone(),
            disable_user_input: None,
            input_examples: None,
            cache_control: None,
        }
    }

    /// Extract text content from Claude response content blocks
    fn extract_text_content(content: &[ContentBlock]) -> String {
        content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Extract tool calls from Claude response content blocks
    fn extract_tool_calls(content: &[ContentBlock]) -> Vec<ToolCall> {
        content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => Some(ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: input.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Convert StopReason enum to string
    fn stop_reason_to_string(reason: Option<StopReason>) -> String {
        match reason {
            Some(StopReason::EndTurn) => "end_turn".to_string(),
            Some(StopReason::MaxTokens) => "max_tokens".to_string(),
            Some(StopReason::StopSequence) => "stop_sequence".to_string(),
            Some(StopReason::ToolUse) => "tool_use".to_string(),
            Some(StopReason::PauseTurn) => "pause_turn".to_string(),
            None => "stop".to_string(),
        }
    }

    /// Build a MessagesRequest with the given messages and optional tools
    fn build_request(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<Tool>>,
        system: Option<&str>,
    ) -> MessagesRequest {
        let mut request = MessagesRequest::new(self.model.clone(), self.max_tokens(), messages);

        // Apply model parameters
        if let Some(temp) = self.params.temperature {
            request = request.with_temperature(temp);
        }
        // Note: top_p is not supported by claude-sdk MessagesRequest

        // Add system prompt if provided
        if let Some(sys) = system {
            request = request.with_system(sys.to_string());
        }

        // Add tools if provided
        if let Some(t) = tools {
            request = request.with_tools(t);
        }

        request
    }
}

#[async_trait]
impl LLMClient for AnthropicClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let messages = vec![Message::user(prompt.to_string())];
        let request = self.build_request(messages, None, None);

        let response = self
            .client
            .send_message(request)
            .await
            .map_err(|e| AppError::LLM(format!("Anthropic API error: {}", e)))?;

        Ok(Self::extract_text_content(&response.content))
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let messages = vec![Message::user(prompt.to_string())];
        let request = self.build_request(messages, None, Some(system));

        let response = self
            .client
            .send_message(request)
            .await
            .map_err(|e| AppError::LLM(format!("Anthropic API error: {}", e)))?;

        Ok(Self::extract_text_content(&response.content))
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<String> {
        let mut system_prompt: Option<String> = None;
        let claude_messages: Vec<Message> = messages
            .iter()
            .filter_map(|(role, content)| match role.as_str() {
                "system" => {
                    // Claude handles system prompts separately
                    system_prompt = Some(content.clone());
                    None
                }
                "assistant" => Some(Message::assistant(content.clone())),
                _ => Some(Message::user(content.clone())), // Default to user
            })
            .collect();

        let request = self.build_request(claude_messages, None, system_prompt.as_deref());

        let response = self
            .client
            .send_message(request)
            .await
            .map_err(|e| AppError::LLM(format!("Anthropic API error: {}", e)))?;

        Ok(Self::extract_text_content(&response.content))
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        let claude_tools: Vec<Tool> = tools.iter().map(Self::convert_tool).collect();
        let messages = vec![Message::user(prompt.to_string())];
        let request = self.build_request(messages, Some(claude_tools), None);

        let response = self
            .client
            .send_message(request)
            .await
            .map_err(|e| AppError::LLM(format!("Anthropic API error: {}", e)))?;

        let content = Self::extract_text_content(&response.content);
        let tool_calls = Self::extract_tool_calls(&response.content);

        // Determine finish reason based on stop_reason
        let finish_reason = Self::stop_reason_to_string(response.stop_reason);

        // Extract token usage
        let usage = Some(TokenUsage::new(
            response.usage.input_tokens as u32,
            response.usage.output_tokens as u32,
        ));

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
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let messages = vec![Message::user(prompt.to_string())];
        let request = self.build_request(messages, None, None);

        let stream = self
            .client
            .send_streaming(request)
            .await
            .map_err(|e| AppError::LLM(format!("Anthropic API error: {}", e)))?;

        let result_stream = async_stream::stream! {
            let mut stream = stream;
            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        // Extract text delta from stream events
                        if let Some(text) = Self::extract_stream_text(&event) {
                            yield Ok(text);
                        }
                    }
                    Err(e) => {
                        yield Err(AppError::LLM(format!("Stream error: {}", e)));
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(result_stream)))
    }

    async fn stream_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let messages = vec![Message::user(prompt.to_string())];
        let request = self.build_request(messages, None, Some(system));

        let stream = self
            .client
            .send_streaming(request)
            .await
            .map_err(|e| AppError::LLM(format!("Anthropic API error: {}", e)))?;

        let result_stream = async_stream::stream! {
            let mut stream = stream;
            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        if let Some(text) = Self::extract_stream_text(&event) {
                            yield Ok(text);
                        }
                    }
                    Err(e) => {
                        yield Err(AppError::LLM(format!("Stream error: {}", e)));
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(result_stream)))
    }

    async fn stream_with_history(
        &self,
        messages: &[(String, String)],
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let mut system_prompt: Option<String> = None;
        let claude_messages: Vec<Message> = messages
            .iter()
            .filter_map(|(role, content)| match role.as_str() {
                "system" => {
                    system_prompt = Some(content.clone());
                    None
                }
                "assistant" => Some(Message::assistant(content.clone())),
                _ => Some(Message::user(content.clone())),
            })
            .collect();

        let request = self.build_request(claude_messages, None, system_prompt.as_deref());

        let stream = self
            .client
            .send_streaming(request)
            .await
            .map_err(|e| AppError::LLM(format!("Anthropic API error: {}", e)))?;

        let result_stream = async_stream::stream! {
            let mut stream = stream;
            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        if let Some(text) = Self::extract_stream_text(&event) {
                            yield Ok(text);
                        }
                    }
                    Err(e) => {
                        yield Err(AppError::LLM(format!("Stream error: {}", e)));
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(result_stream)))
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

impl AnthropicClient {
    /// Extract text from a streaming event
    fn extract_stream_text(event: &StreamEvent) -> Option<String> {
        match event {
            StreamEvent::ContentBlockDelta { delta, .. } => delta.text().map(|s| s.to_string()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = AnthropicClient::new(
            "test-key".to_string(),
            "claude-3-5-sonnet-20241022".to_string(),
        );

        assert_eq!(client.model_name(), "claude-3-5-sonnet-20241022");
    }

    #[test]
    fn test_client_with_params() {
        let params = ModelParams {
            temperature: Some(0.7),
            max_tokens: Some(2048),
            top_p: Some(0.9),
            frequency_penalty: None,
            presence_penalty: None,
        };

        let client = AnthropicClient::with_params(
            "test-key".to_string(),
            "claude-3-5-sonnet-20241022".to_string(),
            params,
        );

        assert_eq!(client.model_name(), "claude-3-5-sonnet-20241022");
        assert_eq!(client.max_tokens(), 2048);
    }

    #[test]
    fn test_tool_conversion() {
        let tool = ToolDefinition {
            name: "calculator".to_string(),
            description: "Performs math operations".to_string(),
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

        let claude_tool = AnthropicClient::convert_tool(&tool);
        assert_eq!(claude_tool.name, "calculator");
        assert_eq!(claude_tool.description, "Performs math operations");
    }

    #[test]
    fn test_stop_reason_conversion() {
        assert_eq!(
            AnthropicClient::stop_reason_to_string(Some(StopReason::EndTurn)),
            "end_turn"
        );
        assert_eq!(
            AnthropicClient::stop_reason_to_string(Some(StopReason::ToolUse)),
            "tool_use"
        );
        assert_eq!(AnthropicClient::stop_reason_to_string(None), "stop");
    }
}
