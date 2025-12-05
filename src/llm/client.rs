//! LLM Client abstractions and provider management
//!
//! This module provides a unified interface for interacting with various LLM providers:
//! - **OpenAI**: Full support including streaming and tool calling
//! - **Ollama**: Full support for local LLM inference with streaming
//! - **Anthropic**: Placeholder (not yet implemented)
//! - **LlamaCpp**: Placeholder (not yet implemented, use Ollama instead)

use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use async_trait::async_trait;

/// Generic LLM client trait for provider abstraction
///
/// All LLM providers implement this trait, allowing for easy swapping
/// between providers without changing application code.
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

/// Response from an LLM generation request
#[derive(Debug, Clone)]
pub struct LLMResponse {
    /// The text content of the response
    pub content: String,
    /// Any tool calls requested by the model
    pub tool_calls: Vec<ToolCall>,
    /// The reason generation stopped (e.g., "stop", "tool_calls", "length")
    pub finish_reason: String,
}

/// Provider enum for runtime selection
///
/// # Supported Providers
///
/// | Provider | Status | Streaming | Tool Calling | Notes |
/// |----------|--------|-----------|--------------|-------|
/// | OpenAI | ✅ Full | ✅ | ✅ | Recommended for production |
/// | Ollama | ✅ Full | ✅ | Basic | Recommended for local |
/// | Anthropic | ❌ Stub | - | - | Not yet implemented |
/// | LlamaCpp | ❌ Stub | - | - | Use Ollama instead |
#[derive(Debug, Clone)]
pub enum Provider {
    /// OpenAI API provider (including Azure OpenAI and compatible APIs)
    ///
    /// # Example
    /// ```rust,ignore
    /// let provider = Provider::OpenAI {
    ///     api_key: "sk-...".to_string(),
    ///     api_base: "https://api.openai.com/v1".to_string(),
    ///     model: "gpt-4o-mini".to_string(),
    /// };
    /// ```
    OpenAI {
        api_key: String,
        api_base: String,
        model: String,
    },

    /// Anthropic Claude API provider
    ///
    /// # Status
    ///
    /// **Not yet implemented.** This is a placeholder for future implementation.
    ///
    /// # Alternatives
    ///
    /// - Use OpenAI provider with OpenRouter (supports Claude models)
    /// - Use the anthropic crate directly in your application
    ///
    /// # Future Implementation
    ///
    /// When implemented, this will support:
    /// - Claude 3 models (Opus, Sonnet, Haiku)
    /// - Streaming responses
    /// - Tool/function calling
    Anthropic { api_key: String, model: String },

    /// Ollama local LLM provider
    ///
    /// # Example
    /// ```rust,ignore
    /// let provider = Provider::Ollama {
    ///     base_url: "http://localhost:11434".to_string(),
    ///     model: "llama3.2".to_string(),
    /// };
    /// ```
    ///
    /// # Recommended Models
    ///
    /// - `llama3.2` - General purpose, good balance of speed and quality
    /// - `mistral` - Fast inference, good for simple tasks
    /// - `codellama` - Optimized for code generation
    /// - `llama3.1` - Supports function calling
    Ollama { base_url: String, model: String },

    /// LlamaCpp direct integration
    ///
    /// # Status
    ///
    /// **Not yet implemented.** This is a placeholder for future implementation.
    ///
    /// # Recommendation
    ///
    /// Use **Ollama** instead for local LLM inference:
    /// - Easier setup (no manual model management)
    /// - Better cross-platform support
    /// - Built-in API server
    ///
    /// # Future Implementation
    ///
    /// When implemented, this will load GGUF model files directly using
    /// the `llama-cpp-2` crate.
    LlamaCpp { model_path: String },
}

impl Provider {
    /// Create a client instance for this provider
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The provider is not yet implemented (Anthropic, LlamaCpp)
    /// - Connection to the provider fails
    /// - Invalid configuration
    pub async fn create_client(&self) -> Result<Box<dyn LLMClient>> {
        match self {
            Provider::OpenAI {
                api_key,
                api_base,
                model,
            } => Ok(Box::new(super::openai::OpenAIClient::new(
                api_key.clone(),
                api_base.clone(),
                model.clone(),
            ))),

            Provider::Anthropic { api_key, model } => {
                // Anthropic support is planned but not yet implemented
                // For now, suggest alternatives
                Err(AppError::LLM(format!(
                    "Anthropic provider not yet implemented. \
                     Requested model: '{}'. \
                     Alternatives: \
                     (1) Use OpenAI provider with OpenRouter for Claude access, \
                     (2) Use Ollama for local inference. \
                     API key provided: {}",
                    model,
                    if api_key.is_empty() { "no" } else { "yes" }
                )))
            }

            Provider::Ollama { base_url, model } => Ok(Box::new(
                super::ollama::OllamaClient::new(base_url.clone(), model.clone()).await?,
            )),

            Provider::LlamaCpp { model_path } => Ok(Box::new(
                super::llamacpp::LlamaCppClient::new(model_path.clone())?,
            )),
        }
    }

    /// Check if this provider is fully implemented
    pub fn is_implemented(&self) -> bool {
        matches!(self, Provider::OpenAI { .. } | Provider::Ollama { .. })
    }

    /// Get a human-readable name for this provider
    pub fn name(&self) -> &'static str {
        match self {
            Provider::OpenAI { .. } => "OpenAI",
            Provider::Anthropic { .. } => "Anthropic",
            Provider::Ollama { .. } => "Ollama",
            Provider::LlamaCpp { .. } => "LlamaCpp",
        }
    }
}

/// Configuration-based client factory
///
/// Provides a convenient way to create LLM clients with a default provider
/// while allowing runtime provider switching.
///
/// # Example
///
/// ```rust,ignore
/// use ares::llm::{LLMClientFactory, Provider};
///
/// let factory = LLMClientFactory::new(Provider::Ollama {
///     base_url: "http://localhost:11434".to_string(),
///     model: "llama3.2".to_string(),
/// });
///
/// // Use default provider
/// let client = factory.create_default().await?;
///
/// // Or use a different provider for this request
/// let openai_client = factory.create_with_provider(Provider::OpenAI {
///     api_key: "sk-...".to_string(),
///     api_base: "https://api.openai.com/v1".to_string(),
///     model: "gpt-4o-mini".to_string(),
/// }).await?;
/// ```
pub struct LLMClientFactory {
    default_provider: Provider,
}

impl LLMClientFactory {
    /// Create a new factory with the specified default provider
    pub fn new(default_provider: Provider) -> Self {
        Self { default_provider }
    }

    /// Create a client using the default provider
    pub async fn create_default(&self) -> Result<Box<dyn LLMClient>> {
        self.default_provider.create_client().await
    }

    /// Create a client using a specific provider
    pub async fn create_with_provider(&self, provider: Provider) -> Result<Box<dyn LLMClient>> {
        provider.create_client().await
    }

    /// Get a reference to the default provider
    pub fn default_provider(&self) -> &Provider {
        &self.default_provider
    }

    /// Check if the default provider is implemented
    pub fn is_default_implemented(&self) -> bool {
        self.default_provider.is_implemented()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_is_implemented() {
        let openai = Provider::OpenAI {
            api_key: "test".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4".to_string(),
        };
        assert!(openai.is_implemented());

        let ollama = Provider::Ollama {
            base_url: "http://localhost:11434".to_string(),
            model: "llama3.2".to_string(),
        };
        assert!(ollama.is_implemented());

        let anthropic = Provider::Anthropic {
            api_key: "test".to_string(),
            model: "claude-3".to_string(),
        };
        assert!(!anthropic.is_implemented());

        let llamacpp = Provider::LlamaCpp {
            model_path: "/path/to/model.gguf".to_string(),
        };
        assert!(!llamacpp.is_implemented());
    }

    #[test]
    fn test_provider_name() {
        let openai = Provider::OpenAI {
            api_key: "".to_string(),
            api_base: "".to_string(),
            model: "".to_string(),
        };
        assert_eq!(openai.name(), "OpenAI");

        let ollama = Provider::Ollama {
            base_url: "".to_string(),
            model: "".to_string(),
        };
        assert_eq!(ollama.name(), "Ollama");
    }

    #[test]
    fn test_factory_default_provider() {
        let provider = Provider::Ollama {
            base_url: "http://localhost:11434".to_string(),
            model: "llama3.2".to_string(),
        };

        let factory = LLMClientFactory::new(provider);
        assert!(factory.is_default_implemented());
        assert_eq!(factory.default_provider().name(), "Ollama");
    }

    #[tokio::test]
    async fn test_anthropic_returns_helpful_error() {
        let provider = Provider::Anthropic {
            api_key: "test-key".to_string(),
            model: "claude-3-sonnet".to_string(),
        };

        let result = provider.create_client().await;
        assert!(result.is_err());

        // Use match instead of unwrap_err since Box<dyn LLMClient> doesn't implement Debug
        let err = match result {
            Ok(_) => panic!("Expected error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("not yet implemented"));
        assert!(err.contains("claude-3-sonnet"));
        assert!(err.contains("OpenRouter"));
    }
}
