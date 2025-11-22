use crate::llm::client::{LLMClient, LLMResponse};
use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use async_trait::async_trait;
use ollama_rs::{
    Ollama,
    generation::chat::{ChatMessage, ChatMessageRequest},
};

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
        // Ollama supports function calling through the tools API
        // For now, return basic response without tool calling
        // TODO: Implement proper tool calling with ollama-rs coordinator
        let content = self.generate(prompt).await?;

        Ok(LLMResponse {
            content,
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
        })
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        // TODO: Implement streaming support
        Err(AppError::LLM(
            "Streaming not yet implemented for Ollama".to_string(),
        ))
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
