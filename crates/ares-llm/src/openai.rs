//! OpenAI LLM client implementation
//!
//! This module provides integration with OpenAI API and compatible endpoints.
//!
//! # Features
//!
//! Enable with the `openai` feature flag.
//!
//! # Example
//!
//! ```rust,ignore
//! use ares::llm::{LLMClient, Provider};
//!
//! let provider = Provider::OpenAI {
//!     api_key: "sk-...".to_string(),
//!     api_base: "https://api.openai.com/v1".to_string(),
//!     model: "gpt-4".to_string(),
//! };
//! let client = provider.create_client().await?;
//! let response = client.generate("Hello!").await?;
//! ```

use crate::client::{LLMClient, LLMResponse, ModelParams, TokenUsage};
use crate::coordinator::{ConversationMessage, MessageRole};
use ares_types::types::{AppError, Result, ToolCall, ToolDefinition};
use async_openai::{
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ReasoningEffort,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestToolMessageArgs,
        ChatCompletionRequestUserMessageArgs, ChatCompletionTool, ChatCompletionTools,
        CreateChatCompletionRequestArgs, FunctionCall, FunctionObject,
    },
    Client,
};
use async_trait::async_trait;
use futures::StreamExt;

/// OpenAI client for API-based inference
pub struct OpenAIClient {
    client: Client<OpenAIConfig>,
    model: String,
    params: ModelParams,
}

impl OpenAIClient {
    /// Create a new OpenAI client
    ///
    /// # Arguments
    ///
    /// * `api_key` - OpenAI API key
    /// * `api_base` - Base URL for the API (e.g., `https://api.openai.com/v1`)
    /// * `model` - Model identifier (e.g., "gpt-4", "gpt-3.5-turbo")
    pub fn new(api_key: String, api_base: String, model: String) -> Self {
        Self::with_params(api_key, api_base, model, ModelParams::default())
    }

    /// Create a new OpenAI client with model parameters
    ///
    /// # Arguments
    ///
    /// * `api_key` - OpenAI API key
    /// * `api_base` - Base URL for the API (e.g., `https://api.openai.com/v1`)
    /// * `model` - Model identifier (e.g., "gpt-4", "gpt-3.5-turbo")
    /// * `params` - Model inference parameters (temperature, max_tokens, etc.)
    pub fn with_params(
        api_key: String,
        api_base: String,
        model: String,
        params: ModelParams,
    ) -> Self {
        let config = OpenAIConfig::new()
            .with_api_key(api_key)
            .with_api_base(api_base);

        Self {
            client: Client::with_config(config),
            model,
            params,
        }
    }

    /// Convert ToolDefinition to ChatCompletionTool
    fn convert_tool(tool: &ToolDefinition) -> ChatCompletionTools {
        ChatCompletionTools::Function(ChatCompletionTool {
            function: FunctionObject {
                name: tool.name.clone(),
                description: Some(tool.description.clone()),
                parameters: Some(tool.parameters.clone()),
                strict: None,
            },
        })
    }

    /// Extract tool calls from the response message tool calls
    fn extract_tool_calls(tool_calls: &[ChatCompletionMessageToolCalls]) -> Vec<ToolCall> {
        tool_calls
            .iter()
            .filter_map(|wrapper| match wrapper {
                ChatCompletionMessageToolCalls::Function(call) => Some(ToolCall {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    arguments: serde_json::from_str(&call.function.arguments)
                        .unwrap_or(serde_json::json!({})),
                }),
                ChatCompletionMessageToolCalls::Custom(_) => None,
            })
            .collect()
    }

    /// Convert a ConversationMessage to OpenAI's ChatCompletionRequestMessage
    fn convert_conversation_message(
        &self,
        msg: &ConversationMessage,
    ) -> Result<ChatCompletionRequestMessage> {
        match msg.role {
            MessageRole::System => {
                let system_msg = ChatCompletionRequestSystemMessageArgs::default()
                    .content(msg.content.clone())
                    .build()
                    .map_err(|e| AppError::LLM(format!("Failed to build system message: {}", e)))?;
                Ok(ChatCompletionRequestMessage::System(system_msg))
            }
            MessageRole::User => {
                let user_msg = ChatCompletionRequestUserMessageArgs::default()
                    .content(msg.content.clone())
                    .build()
                    .map_err(|e| AppError::LLM(format!("Failed to build user message: {}", e)))?;
                Ok(ChatCompletionRequestMessage::User(user_msg))
            }
            MessageRole::Assistant => {
                let mut builder = ChatCompletionRequestAssistantMessageArgs::default();

                if !msg.content.is_empty() {
                    builder.content(msg.content.clone());
                }

                // Convert tool calls if present
                if !msg.tool_calls.is_empty() {
                    let openai_tool_calls: Vec<ChatCompletionMessageToolCalls> = msg
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            ChatCompletionMessageToolCalls::Function(
                                ChatCompletionMessageToolCall {
                                    id: tc.id.clone(),
                                    function: FunctionCall {
                                        name: tc.name.clone(),
                                        arguments: serde_json::to_string(&tc.arguments)
                                            .unwrap_or_else(|_| "{}".to_string()),
                                    },
                                },
                            )
                        })
                        .collect();
                    builder.tool_calls(openai_tool_calls);
                }

                let assistant_msg = builder.build().map_err(|e| {
                    AppError::LLM(format!("Failed to build assistant message: {}", e))
                })?;
                Ok(ChatCompletionRequestMessage::Assistant(assistant_msg))
            }
            MessageRole::Tool => {
                let tool_call_id = msg.tool_call_id.clone().ok_or_else(|| {
                    AppError::LLM("Tool message must have a tool_call_id".to_string())
                })?;

                let tool_msg = ChatCompletionRequestToolMessageArgs::default()
                    .tool_call_id(tool_call_id)
                    .content(msg.content.clone())
                    .build()
                    .map_err(|e| AppError::LLM(format!("Failed to build tool message: {}", e)))?;
                Ok(ChatCompletionRequestMessage::Tool(tool_msg))
            }
        }
    }

    fn apply_model_params(&self, builder: &mut CreateChatCompletionRequestArgs) {
        // GPT-5 chat-completions works reliably here when we explicitly cap reasoning effort.
        if self.model.starts_with("gpt-5") {
            builder.reasoning_effort(ReasoningEffort::Low);
        }

        if let Some(temp) = self.params.temperature {
            builder.temperature(temp);
        }
        if let Some(max_tokens) = self.params.max_tokens {
            builder.max_completion_tokens(max_tokens);
        }
        if let Some(top_p) = self.params.top_p {
            builder.top_p(top_p);
        }
        if let Some(freq_penalty) = self.params.frequency_penalty {
            builder.frequency_penalty(freq_penalty);
        }
        if let Some(pres_penalty) = self.params.presence_penalty {
            builder.presence_penalty(pres_penalty);
        }
    }
}

#[async_trait]
impl LLMClient for OpenAIClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let message = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build message: {}", e)))?;

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(vec![ChatCompletionRequestMessage::User(message)]);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .ok_or_else(|| AppError::LLM("No response from OpenAI".to_string()))
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let system_message = ChatCompletionRequestSystemMessageArgs::default()
            .content(system)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build system message: {}", e)))?;

        let user_message = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build user message: {}", e)))?;

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(vec![
            ChatCompletionRequestMessage::System(system_message),
            ChatCompletionRequestMessage::User(user_message),
        ]);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .ok_or_else(|| AppError::LLM("No response from OpenAI".to_string()))
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<LLMResponse> {
        let chat_messages: std::result::Result<Vec<ChatCompletionRequestMessage>, AppError> =
            messages
                .iter()
                .map(|(role, content)| {
                    match role.as_str() {
                        "system" => {
                            let msg = ChatCompletionRequestSystemMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!("Failed to build system message: {}", e))
                                })?;
                            Ok(ChatCompletionRequestMessage::System(msg))
                        }
                        "assistant" => {
                            let msg = ChatCompletionRequestAssistantMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!(
                                        "Failed to build assistant message: {}",
                                        e
                                    ))
                                })?;
                            Ok(ChatCompletionRequestMessage::Assistant(msg))
                        }
                        _ => {
                            // Default to user message
                            let msg = ChatCompletionRequestUserMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!("Failed to build user message: {}", e))
                                })?;
                            Ok(ChatCompletionRequestMessage::User(msg))
                        }
                    }
                })
                .collect();

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(chat_messages?);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        let content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .ok_or_else(|| AppError::LLM("No response from OpenAI".to_string()))?;

        #[allow(clippy::unnecessary_cast)]
        let usage = response
            .usage
            .map(|u| TokenUsage::new(u.prompt_tokens as u32, u.completion_tokens as u32));

        Ok(LLMResponse {
            content,
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
        let openai_tools: Vec<ChatCompletionTools> = tools.iter().map(Self::convert_tool).collect();

        let user_message = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build user message: {}", e)))?;

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(vec![ChatCompletionRequestMessage::User(user_message)]);
        builder.tools(openai_tools);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| AppError::LLM("No response from OpenAI".to_string()))?;

        let content = choice.message.content.clone().unwrap_or_default();

        let finish_reason = choice
            .finish_reason
            .as_ref()
            .map(|r| format!("{:?}", r).to_lowercase())
            .unwrap_or_else(|| "stop".to_string());

        let tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .map(|calls| Self::extract_tool_calls(calls))
            .unwrap_or_default();

        // Extract token usage if available
        #[allow(clippy::unnecessary_cast)]
        let usage = response
            .usage
            .map(|u| TokenUsage::new(u.prompt_tokens as u32, u.completion_tokens as u32));

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
        // Convert ConversationMessage to OpenAI format
        let openai_messages: Vec<ChatCompletionRequestMessage> = messages
            .iter()
            .map(|msg| self.convert_conversation_message(msg))
            .collect::<Result<Vec<_>>>()?;

        // Convert tools to OpenAI format
        let openai_tools: Vec<ChatCompletionTools> = tools.iter().map(Self::convert_tool).collect();

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(openai_messages);

        if !openai_tools.is_empty() {
            builder.tools(openai_tools);
        }
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| AppError::LLM("No response from OpenAI".to_string()))?;

        let content = choice.message.content.clone().unwrap_or_default();

        let finish_reason = choice
            .finish_reason
            .as_ref()
            .map(|r| format!("{:?}", r).to_lowercase())
            .unwrap_or_else(|| "stop".to_string());

        let tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .map(|calls| Self::extract_tool_calls(calls))
            .unwrap_or_default();

        #[allow(clippy::unnecessary_cast)]
        let usage = response
            .usage
            .map(|u| TokenUsage::new(u.prompt_tokens as u32, u.completion_tokens as u32));

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
        let user_message = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build user message: {}", e)))?;

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(vec![ChatCompletionRequestMessage::User(user_message)]);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let mut stream = self
            .client
            .chat()
            .create_stream(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        let result_stream = async_stream::stream! {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(response) => {
                        for choice in response.choices {
                            if let Some(content) = choice.delta.content {
                                yield Ok(content);
                            }
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
        let system_message = ChatCompletionRequestSystemMessageArgs::default()
            .content(system)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build system message: {}", e)))?;

        let user_message = ChatCompletionRequestUserMessageArgs::default()
            .content(prompt)
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build user message: {}", e)))?;

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(vec![
            ChatCompletionRequestMessage::System(system_message),
            ChatCompletionRequestMessage::User(user_message),
        ]);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let mut stream = self
            .client
            .chat()
            .create_stream(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        let result_stream = async_stream::stream! {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(response) => {
                        for choice in response.choices {
                            if let Some(content) = choice.delta.content {
                                yield Ok(content);
                            }
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
        let chat_messages: std::result::Result<Vec<ChatCompletionRequestMessage>, AppError> =
            messages
                .iter()
                .map(|(role, content)| {
                    match role.as_str() {
                        "system" => {
                            let msg = ChatCompletionRequestSystemMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!("Failed to build system message: {}", e))
                                })?;
                            Ok(ChatCompletionRequestMessage::System(msg))
                        }
                        "assistant" => {
                            let msg = ChatCompletionRequestAssistantMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!(
                                        "Failed to build assistant message: {}",
                                        e
                                    ))
                                })?;
                            Ok(ChatCompletionRequestMessage::Assistant(msg))
                        }
                        _ => {
                            // Default to user message
                            let msg = ChatCompletionRequestUserMessageArgs::default()
                                .content(content.as_str())
                                .build()
                                .map_err(|e| {
                                    AppError::LLM(format!("Failed to build user message: {}", e))
                                })?;
                            Ok(ChatCompletionRequestMessage::User(msg))
                        }
                    }
                })
                .collect();

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(&self.model);
        builder.messages(chat_messages?);
        self.apply_model_params(&mut builder);

        let request = builder
            .build()
            .map_err(|e| AppError::LLM(format!("Failed to build request: {}", e)))?;

        let mut stream = self
            .client
            .chat()
            .create_stream(request)
            .await
            .map_err(|e| AppError::LLM(format!("OpenAI API error: {}", e)))?;

        let result_stream = async_stream::stream! {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(response) => {
                        for choice in response.choices {
                            if let Some(content) = choice.delta.content {
                                yield Ok(content);
                            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = OpenAIClient::new(
            "test-key".to_string(),
            "https://api.openai.com/v1".to_string(),
            "gpt-4".to_string(),
        );

        assert_eq!(client.model_name(), "gpt-4");
    }

    fn sample_client() -> OpenAIClient {
        OpenAIClient::new(
            "test-key".to_string(),
            "https://api.openai.com/v1".to_string(),
            "gpt-4".to_string(),
        )
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

        let openai_tool = OpenAIClient::convert_tool(&tool);
        match openai_tool {
            ChatCompletionTools::Function(chat_tool) => {
                assert_eq!(chat_tool.function.name, "calculator");
                assert_eq!(
                    chat_tool.function.description,
                    Some("Performs math operations".to_string())
                );
                assert_eq!(
                    chat_tool.function.parameters,
                    Some(tool.parameters.clone())
                );
            }
            ChatCompletionTools::Custom(_) => {
                panic!("Expected Function variant, got Custom");
            }
        }
    }

    #[test]
    fn test_with_params_preserves_model_name() {
        let client = OpenAIClient::with_params(
            "key".to_string(),
            "http://localhost:8000/v1".to_string(),
            "gpt-4o-mini".to_string(),
            ModelParams {
                temperature: Some(0.2),
                max_tokens: Some(256),
                ..Default::default()
            },
        );
        assert_eq!(client.model_name(), "gpt-4o-mini");
    }

    #[test]
    fn test_extract_tool_calls_parses_function_calls() {
        let calls = vec![ChatCompletionMessageToolCalls::Function(
            ChatCompletionMessageToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "calculator".to_string(),
                    arguments: r#"{"a":1,"b":2}"#.to_string(),
                },
            },
        )];
        let extracted = OpenAIClient::extract_tool_calls(&calls);
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].id, "call_1");
        assert_eq!(extracted[0].name, "calculator");
        assert_eq!(extracted[0].arguments["a"], 1);
    }

    #[test]
    fn test_extract_tool_calls_invalid_json_defaults_to_empty_object() {
        let calls = vec![ChatCompletionMessageToolCalls::Function(
            ChatCompletionMessageToolCall {
                id: "call_bad".to_string(),
                function: FunctionCall {
                    name: "broken".to_string(),
                    arguments: "not-json".to_string(),
                },
            },
        )];
        let extracted = OpenAIClient::extract_tool_calls(&calls);
        assert_eq!(extracted[0].arguments, serde_json::json!({}));
    }

    #[test]
    fn test_extract_tool_calls_skips_custom_variant() {
        use async_openai::types::chat::{ChatCompletionMessageCustomToolCall, CustomTool};
        let calls = vec![ChatCompletionMessageToolCalls::Custom(
            ChatCompletionMessageCustomToolCall {
                id: "custom_1".to_string(),
                custom_tool: CustomTool {
                    name: "x".to_string(),
                    input: "{}".to_string(),
                },
            },
        )];
        assert!(OpenAIClient::extract_tool_calls(&calls).is_empty());
    }

    #[test]
    fn test_convert_conversation_message_roles() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let system = ConversationMessage {
            role: MessageRole::System,
            content: "Be helpful".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let user = ConversationMessage {
            role: MessageRole::User,
            content: "Hi".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let assistant = ConversationMessage {
            role: MessageRole::Assistant,
            content: "Hello".to_string(),
            tool_calls: vec![ToolCall {
                id: "tc1".to_string(),
                name: "search".to_string(),
                arguments: serde_json::json!({"q": "rust"}),
            }],
            tool_call_id: None,
        };
        let tool = ConversationMessage {
            role: MessageRole::Tool,
            content: "done".to_string(),
            tool_calls: vec![],
            tool_call_id: Some("tc1".to_string()),
        };

        assert!(matches!(
            client.convert_conversation_message(&system).unwrap(),
            ChatCompletionRequestMessage::System(_)
        ));
        assert!(matches!(
            client.convert_conversation_message(&user).unwrap(),
            ChatCompletionRequestMessage::User(_)
        ));
        assert!(matches!(
            client.convert_conversation_message(&assistant).unwrap(),
            ChatCompletionRequestMessage::Assistant(_)
        ));
        assert!(matches!(
            client.convert_conversation_message(&tool).unwrap(),
            ChatCompletionRequestMessage::Tool(_)
        ));
    }

    #[test]
    fn test_convert_conversation_message_tool_requires_call_id() {
        use crate::coordinator::{ConversationMessage, MessageRole};

        let client = sample_client();
        let msg = ConversationMessage {
            role: MessageRole::Tool,
            content: "result".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
        };
        let err = client.convert_conversation_message(&msg).unwrap_err();
        match err {
            AppError::LLM(msg) => assert!(msg.contains("tool_call_id")),
            other => panic!("expected LLM error, got {other:?}"),
        }
    }

    #[test]
    fn test_openai_provider_config_serde_roundtrip() {
        use ares_config::toml_config::ProviderConfig;

        let original = ProviderConfig::OpenAI {
            api_key_env: "OPENAI_API_KEY".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            default_model: "gpt-4".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(original.type_name(), decoded.type_name());
    }

    #[test]
    fn test_openai_from_config_missing_api_key_env() {
        use ares_config::toml_config::ProviderConfig;
        use crate::client::Provider;

        std::env::remove_var("TEST_OPENAI_MISSING_KEY_OPENAI_RS");
        let config = ProviderConfig::OpenAI {
            api_key_env: "TEST_OPENAI_MISSING_KEY_OPENAI_RS".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            default_model: "gpt-4".to_string(),
        };
        let err = Provider::from_config(&config, None).unwrap_err();
        match err {
            AppError::Configuration(msg) => {
                assert!(msg.contains("TEST_OPENAI_MISSING_KEY_OPENAI_RS"));
            }
            other => panic!("expected Configuration error, got {other:?}"),
        }
    }
}
