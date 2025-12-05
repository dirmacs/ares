use crate::llm::client::{LLMClient, LLMResponse};
use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestUserMessage, ChatCompletionTool, ChatCompletionToolChoiceOption,
        ChatCompletionToolType, CreateChatCompletionRequestArgs,
    },
};
use async_trait::async_trait;
use futures::StreamExt;

pub struct OpenAIClient {
    client: Client<OpenAIConfig>,
    model: String,
}

impl OpenAIClient {
    pub fn new(api_key: String, api_base: String, model: String) -> Self {
        let config = OpenAIConfig::new()
            .with_api_key(api_key)
            .with_api_base(api_base);

        Self {
            client: Client::with_config(config),
            model,
        }
    }
}

#[async_trait]
impl LLMClient for OpenAIClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.model)
            .messages(vec![ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage::from(prompt.to_string()),
            )])
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
        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.model)
            .messages(vec![
                ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage::from(
                    system.to_string(),
                )),
                ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage::from(
                    prompt.to_string(),
                )),
            ])
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
        let chat_messages: Vec<ChatCompletionRequestMessage> = messages
            .iter()
            .map(|(role, content)| match role.as_str() {
                "system" => ChatCompletionRequestMessage::System(
                    ChatCompletionRequestSystemMessage::from(content.clone()),
                ),
                "user" => ChatCompletionRequestMessage::User(
                    ChatCompletionRequestUserMessage::from(content.clone()),
                ),
                _ => ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage::from(
                    content.clone(),
                )),
            })
            .collect();

        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.model)
            .messages(chat_messages)
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
        let openai_tools: Vec<ChatCompletionTool> = tools
            .iter()
            .map(|tool| ChatCompletionTool {
                r#type: ChatCompletionToolType::Function,
                function: async_openai::types::FunctionObject {
                    name: tool.name.clone(),
                    description: Some(tool.description.clone()),
                    parameters: Some(tool.parameters.clone()),
                    strict: None,
                },
            })
            .collect();

        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.model)
            .messages(vec![ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage::from(prompt.to_string()),
            )])
            .tools(openai_tools)
            .tool_choice(ChatCompletionToolChoiceOption::Auto)
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
            .map(|r| format!("{:?}", r))
            .unwrap_or_else(|| "unknown".to_string());

        let tool_calls = if let Some(calls) = &choice.message.tool_calls {
            calls
                .iter()
                .map(|call| ToolCall {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    arguments: serde_json::from_str(&call.function.arguments)
                        .unwrap_or(serde_json::json!({})),
                })
                .collect()
        } else {
            vec![]
        };

        Ok(LLMResponse {
            content,
            tool_calls,
            finish_reason,
        })
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.model)
            .messages(vec![ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage::from(prompt.to_string()),
            )])
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
