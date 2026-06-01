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

use crate::client::{LLMClient, LLMResponse, ModelParams, TokenUsage};
use crate::coordinator::{ConversationMessage, MessageRole};
use ares_types::types::{AppError, Result, ToolCall, ToolDefinition};
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

    /// Convert a ConversationMessage to Claude's Message format
    ///
    /// Claude handles system prompts separately, so they are extracted to the system_prompt parameter.
    /// Tool results are sent as user messages with tool_result content blocks.
    fn convert_conversation_message(
        &self,
        msg: &ConversationMessage,
        system_prompt: &mut Option<String>,
    ) -> Option<Message> {
        match msg.role {
            MessageRole::System => {
                // Claude handles system prompts separately
                *system_prompt = Some(msg.content.clone());
                None
            }
            MessageRole::User => Some(Message::user(msg.content.clone())),
            MessageRole::Assistant => {
                // For assistant messages with tool calls, we need to include the tool_use blocks
                if !msg.tool_calls.is_empty() {
                    let mut content_blocks: Vec<ContentBlock> = Vec::new();

                    // If there's also text content, prepend it
                    if !msg.content.is_empty() {
                        content_blocks.push(ContentBlock::Text {
                            text: msg.content.clone(),
                            cache_control: None,
                            citations: None,
                        });
                    }

                    // Add tool use blocks
                    for tc in &msg.tool_calls {
                        content_blocks.push(ContentBlock::ToolUse {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            input: tc.arguments.clone(),
                            cache_control: None,
                        });
                    }

                    Some(Message {
                        role: claude_sdk::Role::Assistant,
                        content: content_blocks,
                    })
                } else {
                    Some(Message::assistant(msg.content.clone()))
                }
            }
            MessageRole::Tool => {
                // Tool results are sent as user messages with tool_result content blocks
                let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();

                Some(Message {
                    role: claude_sdk::Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: tool_call_id,
                        content: Some(msg.content.clone()),
                        is_error: None,
                    }],
                })
            }
        }
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

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<LLMResponse> {
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

        Ok(LLMResponse {
            content: Self::extract_text_content(&response.content),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
            usage: Some(TokenUsage::new(response.usage.input_tokens as u32, response.usage.output_tokens as u32)),
        })
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

    async fn generate_with_tools_and_history(
        &self,
        messages: &[ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        let claude_tools: Vec<Tool> = if tools.is_empty() {
            vec![]
        } else {
            tools.iter().map(Self::convert_tool).collect()
        };

        // Extract system prompt and convert messages
        let mut system_prompt: Option<String> = None;
        let claude_messages: Vec<Message> = messages
            .iter()
            .filter_map(|msg| self.convert_conversation_message(msg, &mut system_prompt))
            .collect();

        let request = self.build_request(
            claude_messages,
            if claude_tools.is_empty() {
                None
            } else {
                Some(claude_tools)
            },
            system_prompt.as_deref(),
        );

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

    fn sample_client() -> AnthropicClient {
        AnthropicClient::new(
            "test-key".to_string(),
            "claude-3-5-sonnet-20241022".to_string(),
        )
    }

    #[test]
    fn test_stop_reason_conversion() {
        assert_eq!(
            AnthropicClient::stop_reason_to_string(Some(StopReason::EndTurn)),
            "end_turn"
        );
        assert_eq!(
            AnthropicClient::stop_reason_to_string(Some(StopReason::MaxTokens)),
            "max_tokens"
        );
        assert_eq!(
            AnthropicClient::stop_reason_to_string(Some(StopReason::StopSequence)),
            "stop_sequence"
        );
        assert_eq!(
            AnthropicClient::stop_reason_to_string(Some(StopReason::ToolUse)),
            "tool_use"
        );
        assert_eq!(
            AnthropicClient::stop_reason_to_string(Some(StopReason::PauseTurn)),
            "pause_turn"
        );
        assert_eq!(AnthropicClient::stop_reason_to_string(None), "stop");
    }

    #[test]
    fn test_max_tokens_defaults_to_1024() {
        let client = sample_client();
        assert_eq!(client.max_tokens(), 1024);
    }

    #[test]
    fn test_extract_text_content_joins_text_blocks() {
        let blocks = vec![
            ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
                citations: None,
            },
            ContentBlock::ToolUse {
                id: "t1".to_string(),
                name: "calc".to_string(),
                input: serde_json::json!({}),
                cache_control: None,
            },
            ContentBlock::Text {
                text: " world".to_string(),
                cache_control: None,
                citations: None,
            },
        ];
        assert_eq!(AnthropicClient::extract_text_content(&blocks), "Hello world");
    }

    #[test]
    fn test_extract_tool_calls_from_content_blocks() {
        let blocks = vec![ContentBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: "calculator".to_string(),
            input: serde_json::json!({"a": 1}),
            cache_control: None,
        }];
        let calls = AnthropicClient::extract_tool_calls(&blocks);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].name, "calculator");
        assert_eq!(calls[0].arguments["a"], 1);
    }

    #[test]
    fn test_build_request_applies_temperature_system_and_tools() {
        let client = AnthropicClient::with_params(
            "key".to_string(),
            "claude-3-5-sonnet-20241022".to_string(),
            ModelParams {
                temperature: Some(0.4),
                max_tokens: Some(512),
                ..Default::default()
            },
        );
        let tools = vec![AnthropicClient::convert_tool(&ToolDefinition {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        })];
        let request = client.build_request(
            vec![Message::user("Hi".to_string())],
            Some(tools),
            Some("You are helpful"),
        );
        assert_eq!(request.model, "claude-3-5-sonnet-20241022");
        assert_eq!(request.max_tokens, 512);
        assert_eq!(request.temperature, Some(0.4));
        use claude_sdk::types::SystemPrompt;
        match &request.system {
            Some(SystemPrompt::String(s)) => assert_eq!(s, "You are helpful"),
            other => panic!("unexpected system prompt: {other:?}"),
        }
        let tools = request.tools.expect("tools should be set");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "search");
    }

    #[test]
    fn test_convert_conversation_message_extracts_system_and_tool_results() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let mut system_prompt = None;
        let system = ConversationMessage {
            role: MessageRole::System,
            content: "Be concise".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        };
        assert!(client
            .convert_conversation_message(&system, &mut system_prompt)
            .is_none());
        assert_eq!(system_prompt.as_deref(), Some("Be concise"));

        let tool = ConversationMessage {
            role: MessageRole::Tool,
            content: "42".to_string(),
            tool_calls: vec![],
            tool_call_id: Some("toolu_9".to_string()),
        };
        let msg = client
            .convert_conversation_message(&tool, &mut system_prompt)
            .expect("tool result message");
        assert!(matches!(msg.content.first(), Some(ContentBlock::ToolResult { .. })));
    }

    #[test]
    fn test_convert_conversation_message_assistant_with_tool_calls() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let mut system_prompt = None;
        let assistant = ConversationMessage {
            role: MessageRole::Assistant,
            content: "Let me check".to_string(),
            tool_calls: vec![ToolCall {
                id: "toolu_1".to_string(),
                name: "search".to_string(),
                arguments: serde_json::json!({"q": "weather"}),
            }],
            tool_call_id: None,
        };
        let msg = client
            .convert_conversation_message(&assistant, &mut system_prompt)
            .expect("assistant tool message");
        assert_eq!(msg.content.len(), 2);
        assert!(matches!(msg.content[0], ContentBlock::Text { .. }));
        assert!(matches!(msg.content[1], ContentBlock::ToolUse { .. }));
    }

    #[test]
    fn test_extract_stream_text_from_delta() {
        use claude_sdk::ContentDelta;

        let event = StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentDelta::TextDelta {
                text: "Hello".to_string(),
            },
        };
        assert_eq!(
            AnthropicClient::extract_stream_text(&event).as_deref(),
            Some("Hello")
        );
        assert!(AnthropicClient::extract_stream_text(&StreamEvent::Ping).is_none());
    }

    #[test]
    fn test_anthropic_provider_config_serde_roundtrip() {
        use ares_config::toml_config::ProviderConfig;

        let original = ProviderConfig::Anthropic {
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            default_model: "claude-3-5-sonnet-20241022".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original.type_name(), decoded.type_name());
    }

    #[test]
    fn test_anthropic_from_config_missing_api_key_env() {
        use ares_config::toml_config::ProviderConfig;
        use crate::client::Provider;

        std::env::remove_var("TEST_ANTHROPIC_MISSING_KEY_ANTHROPIC_RS");
        let config = ProviderConfig::Anthropic {
            api_key_env: "TEST_ANTHROPIC_MISSING_KEY_ANTHROPIC_RS".to_string(),
            default_model: "claude-3-5-sonnet-20241022".to_string(),
        };
        let err = Provider::from_config(&config, None).unwrap_err();
        match err {
            AppError::Configuration(msg) => {
                assert!(msg.contains("TEST_ANTHROPIC_MISSING_KEY_ANTHROPIC_RS"));
            }
            other => panic!("expected Configuration error, got {other:?}"),
        }
    }
    #[test]
    fn test_extract_text_content_empty() {
        assert_eq!(AnthropicClient::extract_text_content(&[]), "");
    }

    #[test]
    fn test_extract_tool_calls_mixed_blocks_and_multiple() {
        let blocks = vec![
            ContentBlock::Text {
                text: "ignore me".to_string(),
                cache_control: None,
                citations: None,
            },
            ContentBlock::ToolUse {
                id: "toolu_a".to_string(),
                name: "search".to_string(),
                input: serde_json::json!({"q": "a"}),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "toolu_b".to_string(),
                name: "calc".to_string(),
                input: serde_json::json!({"x": 2}),
                cache_control: None,
            },
        ];
        let calls = AnthropicClient::extract_tool_calls(&blocks);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[1].name, "calc");
    }

    #[test]
    fn test_build_request_minimal_defaults() {
        let client = sample_client();
        let request = client.build_request(vec![Message::user("Hello")], None, None);
        assert_eq!(request.model, "claude-3-5-sonnet-20241022");
        assert_eq!(request.max_tokens, 1024);
        assert!(request.temperature.is_none());
        assert!(request.system.is_none());
        assert!(request.tools.is_none());
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn test_build_request_with_empty_tools_vec() {
        let client = sample_client();
        let request = client.build_request(vec![Message::user("Hi")], Some(vec![]), None);
        assert!(request.tools.is_some());
        assert!(request.tools.unwrap().is_empty());
    }

    #[test]
    fn test_convert_conversation_message_user_role() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let mut system_prompt = None;
        let user = ConversationMessage {
            role: MessageRole::User,
            content: "What is 2+2?".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let msg = client
            .convert_conversation_message(&user, &mut system_prompt)
            .expect("user message");
        assert_eq!(msg.role, claude_sdk::Role::User);
        assert!(matches!(msg.content.first(), Some(ContentBlock::Text { .. })));
    }

    #[test]
    fn test_convert_conversation_message_assistant_text_only() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let mut system_prompt = None;
        let assistant = ConversationMessage {
            role: MessageRole::Assistant,
            content: "Four.".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let msg = client
            .convert_conversation_message(&assistant, &mut system_prompt)
            .expect("assistant message");
        assert_eq!(msg.role, claude_sdk::Role::Assistant);
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn test_convert_conversation_message_assistant_tool_calls_without_text() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let mut system_prompt = None;
        let assistant = ConversationMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "toolu_only".to_string(),
                name: "lookup".to_string(),
                arguments: serde_json::json!({"id": 1}),
            }],
            tool_call_id: None,
        };
        let msg = client
            .convert_conversation_message(&assistant, &mut system_prompt)
            .expect("assistant tool-only message");
        assert_eq!(msg.content.len(), 1);
        assert!(matches!(msg.content[0], ContentBlock::ToolUse { .. }));
    }

    #[test]
    fn test_convert_conversation_message_tool_result_default_id() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let mut system_prompt = None;
        let tool = ConversationMessage {
            role: MessageRole::Tool,
            content: "done".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let msg = client
            .convert_conversation_message(&tool, &mut system_prompt)
            .expect("tool message");
        match &msg.content[0] {
            ContentBlock::ToolResult { tool_use_id, content, .. } => {
                assert!(tool_use_id.is_empty());
                assert_eq!(content.as_deref(), Some("done"));
            }
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn test_convert_tool_copies_parameters_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "query": { "type": "string" } }
        });
        let tool = ToolDefinition {
            name: "web_search".to_string(),
            description: "Search the web".to_string(),
            parameters: schema.clone(),
        };
        let converted = AnthropicClient::convert_tool(&tool);
        assert_eq!(converted.input_schema, schema);
        assert!(converted.disable_user_input.is_none());
    }

    #[test]
    fn test_new_uses_default_model_params() {
        let from_new = AnthropicClient::new("key".to_string(), "model-a".to_string());
        let from_params = AnthropicClient::with_params(
            "key".to_string(),
            "model-a".to_string(),
            ModelParams::default(),
        );
        assert_eq!(from_new.model_name(), from_params.model_name());
        assert_eq!(from_new.max_tokens(), from_params.max_tokens());
    }

    #[test]
    fn test_extract_stream_text_non_text_delta() {
        use claude_sdk::ContentDelta;

        let event = StreamEvent::ContentBlockDelta {
            index: 0,
            delta: ContentDelta::InputJsonDelta {
                partial_json: r#"{"a":"#.to_string(),
            },
        };
        assert!(AnthropicClient::extract_stream_text(&event).is_none());
    }

    #[test]
    fn test_extract_stream_text_other_events_return_none() {
        use claude_sdk::{MessageDelta, Usage};

        let usage = Usage {
            input_tokens: 1,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let message_start: StreamEvent = serde_json::from_str(
            r#"{
                "type": "message_start",
                "message": {
                    "id": "msg_1",
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": "claude-3-5-sonnet-20241022",
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 1, "output_tokens": 0 }
                }
            }"#,
        )
        .unwrap();
        let stream_error: StreamEvent = serde_json::from_str(
            r#"{
                "type": "error",
                "error": {
                    "type": "overloaded_error",
                    "message": "rate limited"
                }
            }"#,
        )
        .unwrap();
        let events = vec![
            message_start,
            StreamEvent::ContentBlockStart {
                index: 0,
                content_block: ContentBlock::Text {
                    text: "Hi".to_string(),
                    cache_control: None,
                    citations: None,
                },
            },
            StreamEvent::ContentBlockStop { index: 0 },
            StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: Some(StopReason::EndTurn),
                    stop_sequence: None,
                },
                usage,
            },
            StreamEvent::MessageStop,
            stream_error,
        ];
        for event in events {
            assert!(AnthropicClient::extract_stream_text(&event).is_none());
        }
    }

    #[test]
    fn test_stream_event_serde_message_start_roundtrip() {
        let json = r#"{
            "type": "message_start",
            "message": {
                "id": "msg_123",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "claude-3-5-sonnet-20241022",
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 10, "output_tokens": 0 }
            }
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, StreamEvent::MessageStart { .. }));
        let encoded = serde_json::to_string(&event).unwrap();
        let decoded: StreamEvent = serde_json::from_str(&encoded).unwrap();
        assert!(matches!(decoded, StreamEvent::MessageStart { .. }));
    }

    #[test]
    fn test_stream_event_malformed_json_returns_error() {
        let err = serde_json::from_str::<StreamEvent>("{not valid json");
        assert!(err.is_err());
    }

    #[test]
    fn test_content_block_tool_use_serde_roundtrip() {
        let block = ContentBlock::ToolUse {
            id: "toolu_99".to_string(),
            name: "grep".to_string(),
            input: serde_json::json!({"pattern": "foo"}),
            cache_control: None,
        };
        let json = serde_json::to_string(&block).unwrap();
        let decoded: ContentBlock = serde_json::from_str(&json).unwrap();
        match decoded {
            ContentBlock::ToolUse { id, name, input, .. } => {
                assert_eq!(id, "toolu_99");
                assert_eq!(name, "grep");
                assert_eq!(input["pattern"], "foo");
            }
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn test_message_user_and_assistant_helpers() {
        let user = Message::user("hello");
        assert_eq!(user.role, claude_sdk::Role::User);
        let assistant = Message::assistant("world");
        assert_eq!(assistant.role, claude_sdk::Role::Assistant);
        let tool_result = Message::tool_result("toolu_1", "result payload");
        assert!(matches!(
            tool_result.content.first(),
            Some(ContentBlock::ToolResult { .. })
        ));
    }

    #[test]
    fn test_stop_reason_serde_all_variants() {
        for reason in [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::StopSequence,
            StopReason::ToolUse,
            StopReason::PauseTurn,
        ] {
            let json = serde_json::to_string(&reason).unwrap();
            let decoded: StopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(reason, decoded);
        }
    }

    #[test]
    fn test_usage_serde_roundtrip() {
        use claude_sdk::Usage;

        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(10),
            cache_read_input_tokens: Some(5),
        };
        let json = serde_json::to_string(&usage).unwrap();
        let decoded: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.input_tokens, 100);
        assert_eq!(decoded.output_tokens, 50);
        assert_eq!(decoded.cache_creation_input_tokens, Some(10));
        assert_eq!(decoded.cache_read_input_tokens, Some(5));
    }

    #[test]
    fn test_messages_response_serde_roundtrip() {
        use claude_sdk::{MessagesResponse, Role, Usage};

        let response = MessagesResponse {
            id: "msg_resp_1".to_string(),
            response_type: "message".to_string(),
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "Done".to_string(),
                cache_control: None,
                citations: None,
            }],
            model: "claude-3-5-sonnet-20241022".to_string(),
            stop_reason: Some(StopReason::EndTurn),
            stop_sequence: None,
            usage: Usage {
                input_tokens: 12,
                output_tokens: 4,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };
        let json = serde_json::to_string(&response).unwrap();
        let decoded: MessagesResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id, "msg_resp_1");
        assert_eq!(decoded.usage.output_tokens, 4);
    }

    #[test]
    fn test_tool_serde_roundtrip() {
        let tool = Tool {
            name: "fetch".to_string(),
            description: "Fetch a URL".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            disable_user_input: None,
            input_examples: None,
            cache_control: None,
        };
        let json = serde_json::to_string(&tool).unwrap();
        let decoded: Tool = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.name, "fetch");
        assert_eq!(decoded.description, "Fetch a URL");
    }

    #[test]
    fn test_anthropic_from_config_success_with_env() {
        use ares_config::toml_config::ProviderConfig;
        use crate::client::Provider;

        const ENV: &str = "TEST_ANTHROPIC_KEY_R34_SUCCESS";
        std::env::set_var(ENV, "sk-ant-test-key");
        let config = ProviderConfig::Anthropic {
            api_key_env: ENV.to_string(),
            default_model: "claude-3-5-sonnet-20241022".to_string(),
        };
        let provider = Provider::from_config(&config, None).unwrap();
        match provider {
            Provider::Anthropic { api_key, model, .. } => {
                assert_eq!(api_key, "sk-ant-test-key");
                assert_eq!(model, "claude-3-5-sonnet-20241022");
            }
            other => panic!("expected Anthropic provider, got {other:?}"),
        }
        std::env::remove_var(ENV);
    }

    #[test]
    fn test_anthropic_from_config_model_override() {
        use ares_config::toml_config::ProviderConfig;
        use crate::client::Provider;

        const ENV: &str = "TEST_ANTHROPIC_KEY_R34_OVERRIDE";
        std::env::set_var(ENV, "sk-ant-override");
        let config = ProviderConfig::Anthropic {
            api_key_env: ENV.to_string(),
            default_model: "claude-3-5-sonnet-20241022".to_string(),
        };
        let provider = Provider::from_config(&config, Some("claude-3-haiku-20240307")).unwrap();
        match provider {
            Provider::Anthropic { model, .. } => {
                assert_eq!(model, "claude-3-haiku-20240307");
            }
            other => panic!("expected Anthropic provider, got {other:?}"),
        }
        std::env::remove_var(ENV);
    }

    #[test]
    fn test_anthropic_from_config_with_params() {
        use ares_config::toml_config::ProviderConfig;
        use crate::client::Provider;

        const ENV: &str = "TEST_ANTHROPIC_KEY_R34_PARAMS";
        std::env::set_var(ENV, "sk-ant-params");
        let config = ProviderConfig::Anthropic {
            api_key_env: ENV.to_string(),
            default_model: "claude-3-5-sonnet-20241022".to_string(),
        };
        let params = ModelParams {
            temperature: Some(0.15),
            max_tokens: Some(777),
            ..Default::default()
        };
        let provider =
            Provider::from_config_with_params(&config, None, params.clone()).unwrap();
        match provider {
            Provider::Anthropic {
                api_key,
                model,
                params: loaded,
            } => {
                assert_eq!(api_key, "sk-ant-params");
                assert_eq!(model, "claude-3-5-sonnet-20241022");
                assert_eq!(loaded.temperature, Some(0.15));
                assert_eq!(loaded.max_tokens, Some(777));
            }
            other => panic!("expected Anthropic provider, got {other:?}"),
        }
        std::env::remove_var(ENV);
    }

    #[test]
    fn test_provider_anthropic_name_and_requires_api_key() {
        use crate::client::Provider;

        let provider = Provider::Anthropic {
            api_key: "key".to_string(),
            model: "claude-3-5-sonnet-20241022".to_string(),
            params: ModelParams::default(),
        };
        assert_eq!(provider.name(), "anthropic");
        assert!(provider.requires_api_key());
        assert!(!provider.is_local());
    }

}
