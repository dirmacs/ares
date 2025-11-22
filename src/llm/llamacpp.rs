use crate::llm::client::{LLMClient, LLMResponse};
use crate::types::{AppError, Result, ToolDefinition};
use async_trait::async_trait;

pub struct LlamaCppClient {
    model_path: String,
}

impl LlamaCppCLient {
    pub fn new(model_path: String) -> Result<Self> {
        // TODO: Initialize llama.cpp model
        // This requires llama-cpp-2 crate which has complex setup
        // Or can integrate the lancor crate here
        Ok(Self { model_path })
    }
}

#[async_trait]
impl LLMClient for LlamaCppClient {
    async fn generate(&self, _prompt: &str) -> Result<String> {
        // TODO: Implement llama.cpp generation
        Err(AppError::LLM(
            "LlamaCpp provider not yet fully implemented".to_string(),
        ))
    }

    async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
        Err(AppError::LLM("LlamaCpp provider not yet fully implemented").to_string())
    }

    async fn generate_with_history(&self, _messages: &[(String, String)]) -> Result<String> {
        Err(AppError::LLM("LlamaCpp provider not yet fully implemented").to_string())
    }

    async fn generate_with_tools(
        &self,
        _prompt: &str,
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        Err(AppError::LLM(
            "LlamaCpp provider not yet fully implemented".to_string(),
        ))
    }

    async fn stream(
        &self,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
        Err(AppError::LLM(
            "LlamaCpp provider not yet fully implemented".to_string(),
        ))
    }

    fn model_name(&self) -> &str {
        &self.model_path
    }
}
