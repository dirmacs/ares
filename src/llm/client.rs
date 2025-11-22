use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use async_trait::async_trait;

/// Generic LLM client trait for provider abstraction
#[async_trait]
pub trait LLMClient: Send + Sync {
    /// Generate a completion from a prompt
    async fn generate(&self, prompt: &str) -> Result<String>;

    /// Generate with system prompt
    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String>;

    /// Generate with conversation history
    async fn generate_with_history(
        &self,
        messages: &[(String, String)], // (role, content) pairs
    ) -> Result<String>;

    /// Generate with tool calling support
    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse>;

    /// Stream a completion
    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>;

    /// Get the model name/identifier
    fn model_name(&self) -> &str;
}

#[derive(Debug, Clone)]
pub struct LLMResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: String,
}

/// Provider enum for runtime selection
#[derive(Debug, Clone)]
pub enum Provider {
    OpenAI { api_key: String, model: String },
    Anthropic { api_key: String, model: String },
    Ollama { base_url: String, model: String },
    LlamaCpp { model_path: String },
}

impl Provider {
    pub async fn create_client(&self) -> Result<Box<dyn LLMClient>> {
        match self {
            Provider::OpenAI { api_key, model } =>
        }
    }
}
