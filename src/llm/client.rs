use crate::types::{AppError, Result, ToolCall, ToolDefinition};
use crate::utils::toml_config::{ModelConfig, ProviderConfig};
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

/// Response from an LLM generation call
#[derive(Debug, Clone)]
pub struct LLMResponse {
    /// The generated text content
    pub content: String,
    /// Any tool calls the model wants to make
    pub tool_calls: Vec<ToolCall>,
    /// Reason the generation finished (e.g., "stop", "tool_calls", "length")
    pub finish_reason: String,
}

/// LLM Provider configuration
///
/// Each variant is feature-gated to ensure only enabled providers are available.
/// Use `Provider::from_env()` to automatically select based on environment variables.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Provider {
    /// OpenAI API and compatible endpoints (e.g., Azure OpenAI, local vLLM)
    #[cfg(feature = "openai")]
    OpenAI {
        /// API key for authentication
        api_key: String,
        /// Base URL for the API (default: <https://api.openai.com/v1>)
        api_base: String,
        /// Model identifier (e.g., "gpt-4", "gpt-3.5-turbo")
        model: String,
    },

    /// Ollama local inference server
    #[cfg(feature = "ollama")]
    Ollama {
        /// Base URL for Ollama API (default: http://localhost:11434)
        base_url: String,
        /// Model name (e.g., "ministral-3:3b", "mistral", "qwen3-vl:2b")
        model: String,
    },

    /// LlamaCpp for direct GGUF model loading
    #[cfg(feature = "llamacpp")]
    LlamaCpp {
        /// Path to the GGUF model file
        model_path: String,
    },
}

impl Provider {
    /// Create an LLM client from this provider configuration
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The provider cannot be initialized
    /// - Required configuration is missing
    /// - Network connectivity issues (for remote providers)
    #[allow(unreachable_patterns)]
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

            #[cfg(feature = "ollama")]
            Provider::Ollama { base_url, model } => Ok(Box::new(
                super::ollama::OllamaClient::new(base_url.clone(), model.clone()).await?,
            )),

            #[cfg(feature = "llamacpp")]
            Provider::LlamaCpp { model_path } => Ok(Box::new(
                super::llamacpp::LlamaCppClient::new(model_path.clone())?,
            )),
            _ => unreachable!("Provider variant not enabled"),
        }
    }

    /// Create a provider from environment variables
    ///
    /// Provider priority (first match wins):
    /// 1. **LlamaCpp** - if `LLAMACPP_MODEL_PATH` is set
    /// 2. **OpenAI** - if `OPENAI_API_KEY` is set
    /// 3. **Ollama** - default fallback for local inference
    ///
    /// # Environment Variables
    ///
    /// ## LlamaCpp
    /// - `LLAMACPP_MODEL_PATH` - Path to GGUF model file (required)
    ///
    /// ## OpenAI
    /// - `OPENAI_API_KEY` - API key (required)
    /// - `OPENAI_API_BASE` - Base URL (default: <https://api.openai.com/v1>)
    /// - `OPENAI_MODEL` - Model name (default: gpt-4)
    ///
    /// ## Ollama
    /// - `OLLAMA_BASE_URL` - Server URL (default: http://localhost:11434)
    /// - `OLLAMA_MODEL` - Model name (default: ministral-3:3b)
    ///
    /// # Errors
    ///
    /// Returns an error if no LLM provider features are enabled or configured.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Set environment variables
    /// std::env::set_var("OLLAMA_MODEL", "ministral-3:3b");
    ///
    /// let provider = Provider::from_env()?;
    /// let client = provider.create_client().await?;
    /// ```
    #[allow(unreachable_code)]
    pub fn from_env() -> Result<Self> {
        // Check for LlamaCpp first (direct GGUF loading - highest priority when configured)
        #[cfg(feature = "llamacpp")]
        if let Ok(model_path) = std::env::var("LLAMACPP_MODEL_PATH") {
            if !model_path.is_empty() {
                return Ok(Provider::LlamaCpp { model_path });
            }
        }

        // Check for OpenAI (requires explicit API key configuration)
        #[cfg(feature = "openai")]
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            if !api_key.is_empty() {
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

        // Ollama as default local inference (no API key required)
        #[cfg(feature = "ollama")]
        {
            // Accept both OLLAMA_URL (preferred) and legacy OLLAMA_BASE_URL
            let base_url = std::env::var("OLLAMA_URL")
                .or_else(|_| std::env::var("OLLAMA_BASE_URL"))
                .unwrap_or_else(|_| "http://localhost:11434".into());
            let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "ministral-3:3b".into());
            return Ok(Provider::Ollama { base_url, model });
        }

        // No provider available
        #[allow(unreachable_code)]
        Err(AppError::Configuration(
            "No LLM provider configured. Enable a feature (ollama, openai, llamacpp) and set the appropriate environment variables.".into(),
        ))
    }

    /// Get the provider name as a string
    #[allow(unreachable_patterns)]
    pub fn name(&self) -> &'static str {
        match self {
            #[cfg(feature = "openai")]
            Provider::OpenAI { .. } => "openai",

            #[cfg(feature = "ollama")]
            Provider::Ollama { .. } => "ollama",

            #[cfg(feature = "llamacpp")]
            Provider::LlamaCpp { .. } => "llamacpp",
            _ => unreachable!("Provider variant not enabled"),
        }
    }

    /// Check if this provider requires an API key
    #[allow(unreachable_patterns)]
    pub fn requires_api_key(&self) -> bool {
        match self {
            #[cfg(feature = "openai")]
            Provider::OpenAI { .. } => true,

            #[cfg(feature = "ollama")]
            Provider::Ollama { .. } => false,

            #[cfg(feature = "llamacpp")]
            Provider::LlamaCpp { .. } => false,
            _ => unreachable!("Provider variant not enabled"),
        }
    }

    /// Check if this provider is local (no network required)
    #[allow(unreachable_patterns)]
    pub fn is_local(&self) -> bool {
        match self {
            #[cfg(feature = "openai")]
            Provider::OpenAI { api_base, .. } => {
                api_base.contains("localhost") || api_base.contains("127.0.0.1")
            }

            #[cfg(feature = "ollama")]
            Provider::Ollama { base_url, .. } => {
                base_url.contains("localhost") || base_url.contains("127.0.0.1")
            }

            #[cfg(feature = "llamacpp")]
            Provider::LlamaCpp { .. } => true,
            _ => unreachable!("Provider variant not enabled"),
        }
    }

    /// Create a provider from TOML configuration
    ///
    /// # Arguments
    ///
    /// * `provider_config` - The provider configuration from ares.toml
    /// * `model_override` - Optional model name to override the provider default
    ///
    /// # Errors
    ///
    /// Returns an error if the provider type doesn't match an enabled feature
    /// or if required environment variables are not set.
    #[allow(unused_variables)]
    pub fn from_config(
        provider_config: &ProviderConfig,
        model_override: Option<&str>,
    ) -> Result<Self> {
        match provider_config {
            #[cfg(feature = "ollama")]
            ProviderConfig::Ollama {
                base_url,
                default_model,
            } => Ok(Provider::Ollama {
                base_url: base_url.clone(),
                model: model_override
                    .map(String::from)
                    .unwrap_or_else(|| default_model.clone()),
            }),

            #[cfg(not(feature = "ollama"))]
            ProviderConfig::Ollama { .. } => Err(AppError::Configuration(
                "Ollama provider configured but 'ollama' feature is not enabled".into(),
            )),

            #[cfg(feature = "openai")]
            ProviderConfig::OpenAI {
                api_key_env,
                api_base,
                default_model,
            } => {
                let api_key = std::env::var(api_key_env).map_err(|_| {
                    AppError::Configuration(format!(
                        "OpenAI API key environment variable '{}' is not set",
                        api_key_env
                    ))
                })?;
                Ok(Provider::OpenAI {
                    api_key,
                    api_base: api_base.clone(),
                    model: model_override
                        .map(String::from)
                        .unwrap_or_else(|| default_model.clone()),
                })
            }

            #[cfg(not(feature = "openai"))]
            ProviderConfig::OpenAI { .. } => Err(AppError::Configuration(
                "OpenAI provider configured but 'openai' feature is not enabled".into(),
            )),

            #[cfg(feature = "llamacpp")]
            ProviderConfig::LlamaCpp { model_path, .. } => Ok(Provider::LlamaCpp {
                model_path: model_path.clone(),
            }),

            #[cfg(not(feature = "llamacpp"))]
            ProviderConfig::LlamaCpp { .. } => Err(AppError::Configuration(
                "LlamaCpp provider configured but 'llamacpp' feature is not enabled".into(),
            )),
        }
    }

    /// Create a provider from a model configuration and its associated provider config
    ///
    /// This is the primary way to create providers from TOML config, as it resolves
    /// the model -> provider reference chain.
    pub fn from_model_config(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
    ) -> Result<Self> {
        Self::from_config(provider_config, Some(&model_config.model))
    }
}

/// Trait abstraction for LLM client factories (useful for mocking in tests)
#[async_trait]
pub trait LLMClientFactoryTrait: Send + Sync {
    /// Get the default provider configuration
    fn default_provider(&self) -> &Provider;

    /// Create an LLM client using the default provider
    async fn create_default(&self) -> Result<Box<dyn LLMClient>>;

    /// Create an LLM client using a specific provider
    async fn create_with_provider(&self, provider: Provider) -> Result<Box<dyn LLMClient>>;
}

/// Configuration-based LLM client factory
///
/// Provides a convenient way to create LLM clients with a default provider
/// while allowing runtime provider switching.
pub struct LLMClientFactory {
    default_provider: Provider,
}

impl LLMClientFactory {
    /// Create a new factory with a specific default provider
    pub fn new(default_provider: Provider) -> Self {
        Self { default_provider }
    }

    /// Create a factory from environment variables
    ///
    /// Uses `Provider::from_env()` to determine the default provider.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            default_provider: Provider::from_env()?,
        })
    }

    /// Get the default provider configuration
    pub fn default_provider(&self) -> &Provider {
        &self.default_provider
    }

    /// Create an LLM client using the default provider
    pub async fn create_default(&self) -> Result<Box<dyn LLMClient>> {
        self.default_provider.create_client().await
    }

    /// Create an LLM client using a specific provider
    pub async fn create_with_provider(&self, provider: Provider) -> Result<Box<dyn LLMClient>> {
        provider.create_client().await
    }
}

#[async_trait]
impl LLMClientFactoryTrait for LLMClientFactory {
    fn default_provider(&self) -> &Provider {
        &self.default_provider
    }

    async fn create_default(&self) -> Result<Box<dyn LLMClient>> {
        self.default_provider.create_client().await
    }

    async fn create_with_provider(&self, provider: Provider) -> Result<Box<dyn LLMClient>> {
        provider.create_client().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llm_response_creation() {
        let response = LLMResponse {
            content: "Hello".to_string(),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
        };

        assert_eq!(response.content, "Hello");
        assert!(response.tool_calls.is_empty());
        assert_eq!(response.finish_reason, "stop");
    }

    #[test]
    fn test_llm_response_with_tool_calls() {
        let tool_calls = vec![
            ToolCall {
                id: "1".to_string(),
                name: "calculator".to_string(),
                arguments: serde_json::json!({"a": 1, "b": 2}),
            },
            ToolCall {
                id: "2".to_string(),
                name: "search".to_string(),
                arguments: serde_json::json!({"query": "test"}),
            },
        ];

        let response = LLMResponse {
            content: "".to_string(),
            tool_calls,
            finish_reason: "tool_calls".to_string(),
        };

        assert_eq!(response.tool_calls.len(), 2);
        assert_eq!(response.tool_calls[0].name, "calculator");
        assert_eq!(response.finish_reason, "tool_calls");
    }

    #[test]
    fn test_factory_creation() {
        // This test just verifies the factory can be created
        // Actual provider tests require feature flags
        #[cfg(feature = "ollama")]
        {
            let factory = LLMClientFactory::new(Provider::Ollama {
                base_url: "http://localhost:11434".to_string(),
                model: "test".to_string(),
            });
            assert_eq!(factory.default_provider().name(), "ollama");
        }
    }

    #[cfg(feature = "ollama")]
    #[test]
    fn test_ollama_provider_properties() {
        let provider = Provider::Ollama {
            base_url: "http://localhost:11434".to_string(),
            model: "ministral-3:3b".to_string(),
        };

        assert_eq!(provider.name(), "ollama");
        assert!(!provider.requires_api_key());
        assert!(provider.is_local());
    }

    #[cfg(feature = "openai")]
    #[test]
    fn test_openai_provider_properties() {
        let provider = Provider::OpenAI {
            api_key: "sk-test".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4".to_string(),
        };

        assert_eq!(provider.name(), "openai");
        assert!(provider.requires_api_key());
        assert!(!provider.is_local());
    }

    #[cfg(feature = "openai")]
    #[test]
    fn test_openai_local_provider() {
        let provider = Provider::OpenAI {
            api_key: "test".to_string(),
            api_base: "http://localhost:8000/v1".to_string(),
            model: "local-model".to_string(),
        };

        assert!(provider.is_local());
    }

    #[cfg(feature = "llamacpp")]
    #[test]
    fn test_llamacpp_provider_properties() {
        let provider = Provider::LlamaCpp {
            model_path: "/path/to/model.gguf".to_string(),
        };

        assert_eq!(provider.name(), "llamacpp");
        assert!(!provider.requires_api_key());
        assert!(provider.is_local());
    }
}
