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
///
/// Each variant is feature-gated to ensure only enabled providers are available.
#[derive(Debug, Clone)]
pub enum Provider {
    #[cfg(feature = "openai")]
    OpenAI {
        api_key: String,
        api_base: String,
        model: String,
    },
    Anthropic {
        api_key: String,
        model: String,
    },
    #[cfg(feature = "ollama")]
    Ollama {
        base_url: String,
        model: String,
    },
    #[cfg(feature = "llamacpp")]
    LlamaCpp {
        model_path: String,
    },
}

impl Provider {
    pub async fn create_client(&self) -> Result<Box<dyn LLMClient>> {
        match self {
            #[cfg(feature = "openai")]
            Provider::OpenAI {
                api_key,
                api_base,
                model,
            } => Ok(Box::new(super::openai::OpenAIClient::new(
                api_key.clone(),
                api_base.clone(),
                model.clone(),
            ))),
            Provider::Anthropic { api_key: _, model: _ } => {
                // TODO: Implement Anthropic client
                Err(AppError::LLM(
                    "Anthropic provider not yet implemented".to_string(),
                ))
            }
            #[cfg(feature = "ollama")]
            Provider::Ollama { base_url, model } => Ok(Box::new(
                super::ollama::OllamaClient::new(base_url.clone(), model.clone()).await?,
            )),
            #[cfg(feature = "llamacpp")]
            Provider::LlamaCpp { model_path } => Ok(Box::new(
                super::llamacpp::LlamaCppClient::new(model_path.clone())?,
            )),
        }
    }

    /// Create a default provider from environment variables
    pub fn from_env() -> Result<Self> {
        // Check for Ollama first (default)
        #[cfg(feature = "ollama")]
        {
            let base_url =
                std::env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".into());
            let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".into());
            return Ok(Provider::Ollama { base_url, model });
        }

        // Check for OpenAI
        #[cfg(feature = "openai")]
        {
            if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
                let api_base = std::env::var("OPENAI_API_BASE")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into());
                let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4".into());
                return Ok(Provider::OpenAI {
                    api_key,
                    api_base,
                    model,
                });
            }
        }

        // Check for LlamaCpp
        #[cfg(feature = "llamacpp")]
        {
            if let Ok(model_path) = std::env::var("LLAMACPP_MODEL_PATH") {
                return Ok(Provider::LlamaCpp { model_path });
            }
        }

        Err(AppError::Configuration(
            "No LLM provider configured. Enable a feature (ollama, openai, llamacpp) and set environment variables.".into(),
        ))
    }
}

/// Configuration-based client factory
pub struct LLMClientFactory {
    default_provider: Provider,
}

impl LLMClientFactory {
    pub fn new(default_provider: Provider) -> Self {
        Self { default_provider }
    }

    /// Create factory from environment variables
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            default_provider: Provider::from_env()?,
        })
    }

    pub async fn create_default(&self) -> Result<Box<dyn LLMClient>> {
        self.default_provider.create_client().await
    }

    pub async fn create_with_provider(&self, provider: Provider) -> Result<Box<dyn LLMClient>> {
        provider.create_client().await
    }
}
