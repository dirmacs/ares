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

use crate::llm::client::{LLMClient, LLMResponse, ModelParams, TokenUsage};
use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use async_openai::{
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionMessageToolCalls, ChatCompletionRequestAssistantMessageArgs,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
        ChatCompletionRequestUserMessageArgs, ChatCompletionTool, ChatCompletionTools,
        CreateChatCompletionRequestArgs, FunctionObject,
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

        // Apply model parameters
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

        // Apply model parameters
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

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<String> {
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

        // Apply model parameters
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

        // Apply model parameters
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

        // Apply model parameters
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

        // Apply model parameters
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

        // Apply model parameters
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
            }
            ChatCompletionTools::Custom(_) => {
                panic!("Expected Function variant, got Custom");
            }
        }
    }
}
