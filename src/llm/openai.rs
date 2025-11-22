use crate::llm::client::{LLMClient, LLMResponse};
use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestUserMessage, ChatCompletionTool, ChatCompletionToolChoiceOption,
        ChatCompletionToolType, CreateChatCompletionRequest, CreateChatCompletionRequestArgs,
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

    // async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
    //     let request = CreateChatCompletionRequestArgs::default()
    //         .model(&self.model)
    //         .messages(vec![
    //             ChatCompletionRequestMessage::System(

    //             )
    //         ])
    // }
}
