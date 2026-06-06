use ares_types::types::{AppError, Result, ToolCall, ToolDefinition};
use ares_config::toml_config::{ModelConfig, ProviderConfig};
use async_trait::async_trait;

/// Generic LLM client trait for provider abstraction
#[async_trait]
pub trait LLMClient: Send + Sync {
    /// Generate a completion from a prompt
    async fn generate(&self, prompt: &str) -> Result<String>;

    /// Generate with system prompt
    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String>;

    /// Generate with conversation history, returning full response with token usage
    async fn generate_with_history(
        &self,
        messages: &[(String, String)], // (role, content) pairs
    ) -> Result<LLMResponse>;

    /// Generate with tool calling support
    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse>;

    /// Generate with conversation history AND tool definitions.
    ///
    /// This is the core method for multi-turn tool calling, combining:
    /// - `generate_with_history()` - conversation context
    /// - `generate_with_tools()` - tool calling capability
    ///
    /// # Arguments
    ///
    /// * `messages` - Conversation history as ConversationMessage structs
    /// * `tools` - Available tool definitions
    ///
    /// # Returns
    ///
    /// An LLMResponse containing the model's reply and any tool calls requested.
    async fn generate_with_tools_and_history(
        &self,
        messages: &[crate::coordinator::ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse>;

    /// Stream a completion
    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>;

    /// Stream a completion with system prompt
    async fn stream_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>;

    /// Stream a completion with conversation history
    async fn stream_with_history(
        &self,
        messages: &[(String, String)], // (role, content) pairs
    ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>;

    /// Get the model name/identifier
    fn model_name(&self) -> &str;
}

/// Token usage statistics from an LLM generation call
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    /// Number of tokens in the prompt/input
    pub prompt_tokens: u32,
    /// Number of tokens in the completion/output
    pub completion_tokens: u32,
    /// Total tokens used (prompt + completion)
    pub total_tokens: u32,
}

impl TokenUsage {
    /// Create a new TokenUsage with the given values
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }
    }
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
    /// Token usage statistics (if provided by the model)
    pub usage: Option<TokenUsage>,
}

/// Model inference parameters
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelParams {
    /// Sampling temperature (0.0 = deterministic, 1.0+ = creative)
    pub temperature: Option<f32>,
    /// Maximum tokens to generate
    pub max_tokens: Option<u32>,
    /// Nucleus sampling parameter
    pub top_p: Option<f32>,
    /// Frequency penalty (-2.0 to 2.0)
    pub frequency_penalty: Option<f32>,
    /// Presence penalty (-2.0 to 2.0)
    pub presence_penalty: Option<f32>,
}

impl ModelParams {
    /// Create params from a ModelConfig
    pub fn from_model_config(config: &ModelConfig) -> Self {
        Self {
            temperature: Some(config.temperature),
            max_tokens: Some(config.max_tokens),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
        }
    }
}

/// LLM Provider configuration
///
/// Each variant is feature-gated to ensure only enabled providers are available.
/// Use `Provider::from_env()` to automatically select based on environment variables.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Provider {
    /// OpenAI API and compatible endpoints (e.g., NVIDIA NIM, Azure OpenAI, local vLLM)
    #[cfg(feature = "openai")]
    OpenAI {
        /// API key for authentication
        api_key: String,
        /// Base URL for the API (default: <https://api.openai.com/v1>)
        api_base: String,
        /// Model identifier (e.g., "gpt-4", "nvidia/nemotron-3-ultra-550b-a55b")
        model: String,
        /// Model inference parameters
        params: ModelParams,
    },

    /// Anthropic Claude API
    #[cfg(feature = "anthropic")]
    Anthropic {
        /// API key for authentication
        api_key: String,
        /// Model identifier (e.g., "claude-3-5-sonnet-20241022")
        model: String,
        /// Model inference parameters
        params: ModelParams,
    },

    /// Local Ollama server
    #[cfg(feature = "ollama")]
    Ollama {
        /// Base URL of the Ollama server (e.g., "http://localhost:11434")
        base_url: String,
        /// Model identifier (e.g., "ministral-3:3b")
        model: String,
        /// Model inference parameters
        params: ModelParams,
    },

    /// In-memory stub for unit tests (no network I/O).
    #[cfg(test)]
    TestStub {
        /// Model label returned by [`LLMClient::model_name`].
        model: String,
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
    pub async fn create_client(&self) -> Result<Box<dyn LLMClient>> {
        match self {
            #[cfg(feature = "openai")]
            Provider::OpenAI {
                api_key,
                api_base,
                model,
                params,
            } => Ok(Box::new(super::openai::OpenAIClient::with_params(
                api_key.clone(),
                api_base.clone(),
                model.clone(),
                params.clone(),
            ))),

            #[cfg(feature = "anthropic")]
            Provider::Anthropic {
                api_key,
                model,
                params,
            } => Ok(Box::new(super::anthropic::AnthropicClient::with_params(
                api_key.clone(),
                model.clone(),
                params.clone(),
            ))),

            #[cfg(feature = "ollama")]
            Provider::Ollama {
                base_url,
                model,
                params,
            } => super::ollama::OllamaClient::with_params(
                base_url.clone(),
                model.clone(),
                params.clone(),
            )
            .await
            .map(|c| Box::new(c) as Box<dyn LLMClient>),

            #[cfg(test)]
            Provider::TestStub { model } => {
                Ok(Box::new(test_support::MockLLMClient::new(model.clone())))
            }
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
    pub fn from_env() -> Result<Self> {
        #[cfg(feature = "openai")]
        {
            if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
                if !api_key.is_empty() {
                    let api_base = std::env::var("OPENAI_API_BASE")
                        .unwrap_or_else(|_| "https://api.openai.com/v1".into());
                    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4".into());
                    return Ok(Provider::OpenAI {
                        api_key,
                        api_base,
                        model,
                        params: ModelParams::default(),
                    });
                }
            }

            // Fallback to NVIDIA API key
            if let Ok(api_key) = std::env::var("NVIDIA_API_KEY") {
                if !api_key.is_empty() {
                    return Ok(Provider::OpenAI {
                        api_key,
                        api_base: "https://integrate.api.nvidia.com/v1".into(),
                        model: "nvidia/nemotron-3-ultra-550b-a55b".into(),
                        params: ModelParams::default(),
                    });
                }
            }
        }

        #[cfg(not(feature = "openai"))]
        return Err(AppError::Configuration(
            "OpenAI feature is not enabled.".into(),
        ));

        Err(AppError::Configuration(
            "No LLM provider configured. Set OPENAI_API_KEY or NVIDIA_API_KEY.".into(),
        ))
    }

    /// Get the provider name as a string
    pub fn name(&self) -> &'static str {
        match self {
            #[cfg(feature = "openai")]
            Provider::OpenAI { .. } => "openai",

            #[cfg(feature = "anthropic")]
            Provider::Anthropic { .. } => "anthropic",

            #[cfg(feature = "ollama")]
            Provider::Ollama { .. } => "ollama",

            #[cfg(test)]
            Provider::TestStub { .. } => "test-stub",
        }
    }

    /// Check if this provider requires an API key
    pub fn requires_api_key(&self) -> bool {
        match self {
            #[cfg(feature = "openai")]
            Provider::OpenAI { .. } => true,

            #[cfg(feature = "anthropic")]
            Provider::Anthropic { .. } => true,

            #[cfg(feature = "ollama")]
            Provider::Ollama { .. } => false,

            #[cfg(test)]
            Provider::TestStub { .. } => false,
        }
    }

    /// Check if this provider is local (no network required)
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

            #[cfg(feature = "anthropic")]
            Provider::Anthropic { .. } => false,

            #[cfg(test)]
            Provider::TestStub { .. } => true,
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
        Self::from_config_with_params(provider_config, model_override, ModelParams::default())
    }

    /// Create a provider from TOML configuration with model parameters
    pub fn from_config_with_params(
        provider_config: &ProviderConfig,
        model_override: Option<&str>,
        params: ModelParams,
    ) -> Result<Self> {
        match provider_config {
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
                    params,
                })
            }

            #[cfg(feature = "anthropic")]
            ProviderConfig::Anthropic {
                api_key_env,
                default_model,
            } => {
                let api_key = std::env::var(api_key_env).map_err(|_| {
                    AppError::Configuration(format!(
                        "Anthropic API key environment variable '{}' is not set",
                        api_key_env
                    ))
                })?;
                Ok(Provider::Anthropic {
                    api_key,
                    model: model_override
                        .map(String::from)
                        .unwrap_or_else(|| default_model.clone()),
                    params,
                })
            }

            #[cfg(feature = "ollama")]
            ProviderConfig::Ollama {
                base_url,
                default_model,
                ..
            } => Ok(Provider::Ollama {
                base_url: base_url.clone(),
                model: model_override
                    .map(String::from)
                    .unwrap_or_else(|| default_model.clone()),
                params,
            }),

            // Catch-all for cfg-disabled variants: return a clear error so
            // the runtime can surface it to the admin or the chat path.
            #[allow(unreachable_patterns)]
            _ => Err(AppError::Configuration(format!(
                "{} provider configured but the corresponding feature is not enabled in this build",
                provider_config.type_name()
            ))),
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
        let params = ModelParams::from_model_config(model_config);
        Self::from_config_with_params(provider_config, Some(&model_config.model), params)
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


/// Test doubles shared across crate unit tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use ares_types::types::ToolDefinition;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Minimal LLM client for pool tests — never performs network I/O.
    pub struct MockLLMClient {
        model: String,
        id: u64,
    }

    impl MockLLMClient {
        pub fn new(model: impl Into<String>) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            Self {
                model: model.into(),
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            }
        }
    }

    #[async_trait]
    impl LLMClient for MockLLMClient {
        async fn generate(&self, _prompt: &str) -> Result<String> {
            Ok(format!("mock-{}", self.id))
        }

        async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
            Ok(format!("mock-{}", self.id))
        }

        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: format!("mock-{}", self.id),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: format!("mock-{}", self.id),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn generate_with_tools_and_history(
            &self,
            _messages: &[crate::coordinator::ConversationMessage],
            _tools: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: format!("mock-{}", self.id),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("mock stream not implemented".into()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("mock stream not implemented".into()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("mock stream not implemented".into()))
        }

        fn model_name(&self) -> &str {
            &self.model
        }
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
            usage: None,
        };

        assert_eq!(response.content, "Hello");
        assert!(response.tool_calls.is_empty());
        assert_eq!(response.finish_reason, "stop");
        assert!(response.usage.is_none());
    }

    #[test]
    fn test_llm_response_with_usage() {
        let usage = TokenUsage::new(100, 50);
        let response = LLMResponse {
            content: "Hello".to_string(),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
            usage: Some(usage),
        };

        assert!(response.usage.is_some());
        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
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
            usage: Some(TokenUsage::new(50, 25)),
        };

        assert_eq!(response.tool_calls.len(), 2);
        assert_eq!(response.tool_calls[0].name, "calculator");
        assert_eq!(response.finish_reason, "tool_calls");
        assert_eq!(response.usage.as_ref().unwrap().total_tokens, 75);
    }

    #[test]
    fn test_factory_creation() {
        // This test just verifies the factory can be created
        // Actual provider tests require feature flags
        #[cfg(feature = "openai")]
        {
            let factory = LLMClientFactory::new(Provider::OpenAI {
                api_key: "sk-test".to_string(),
                api_base: "https://api.openai.com/v1".to_string(),
                model: "test".to_string(),
                params: ModelParams::default(),
            });
            assert_eq!(factory.default_provider().name(), "openai");
        }
    }


    #[cfg(feature = "openai")]
    #[test]
    fn test_openai_provider_properties() {
        let provider = Provider::OpenAI {
            api_key: "sk-test".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "gpt-4".to_string(),
            params: ModelParams::default(),
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
            params: ModelParams::default(),
        };

        assert!(provider.is_local());
    }


    // ===== TokenUsage tests =====

    #[test]
    fn test_token_usage_default_all_zeros() {
        let usage = TokenUsage::default();
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn test_token_usage_new_calculates_total() {
        let usage = TokenUsage::new(100, 50);
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
    }

    #[test]
    fn test_token_usage_new_zero_tokens() {
        let usage = TokenUsage::new(0, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn test_token_usage_new_large_values() {
        let usage = TokenUsage::new(u32::MAX / 2, u32::MAX / 2 + 1);
        assert_eq!(usage.total_tokens, u32::MAX);
    }

    #[test]
    fn test_token_usage_serde_roundtrip() {
        let usage = TokenUsage::new(100, 200);
        let json = serde_json::to_string(&usage).unwrap();
        let deserialized: TokenUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(usage, deserialized);
    }

    #[test]
    fn test_token_usage_serde_default_values() {
        let json = r#"{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}"#;
        let usage: TokenUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage, TokenUsage::default());
    }

    #[test]
    fn test_token_usage_serde_partial_json() {
        // All fields present but verify deserialization accepts correct types
        let json = r#"{"prompt_tokens":42,"completion_tokens":58,"total_tokens":100}"#;
        let usage: TokenUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.prompt_tokens, 42);
        assert_eq!(usage.completion_tokens, 58);
        assert_eq!(usage.total_tokens, 100);
    }

    #[test]
    fn test_token_usage_clone_eq() {
        let a = TokenUsage::new(10, 20);
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn test_token_usage_debug_format() {
        let usage = TokenUsage::new(1, 2);
        let debug_str = format!("{:?}", usage);
        assert!(debug_str.contains("TokenUsage"));
        assert!(debug_str.contains("prompt_tokens"));
    }

    // ===== ModelParams tests =====

    #[test]
    fn test_model_params_default_all_none() {
        let params = ModelParams::default();
        assert!(params.temperature.is_none());
        assert!(params.max_tokens.is_none());
        assert!(params.top_p.is_none());
        assert!(params.frequency_penalty.is_none());
        assert!(params.presence_penalty.is_none());
    }

    #[test]
    fn test_model_params_from_model_config_all_fields() {
        let config = ModelConfig {
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            temperature: 0.5,
            max_tokens: 1024,
        };
        let params = ModelParams::from_model_config(&config);
        assert_eq!(params.temperature, Some(0.5));
        assert_eq!(params.max_tokens, Some(1024));
        assert!(params.top_p.is_none());
        assert!(params.frequency_penalty.is_none());
        assert!(params.presence_penalty.is_none());
    }

    #[test]
    fn test_model_params_from_model_config_optional_none() {
        let config = ModelConfig {
            provider: "openai".to_string(),
            model: "mistral".to_string(),
            temperature: 0.7,
            max_tokens: 512,
        };
        let params = ModelParams::from_model_config(&config);
        assert_eq!(params.temperature, Some(0.7));
        assert_eq!(params.max_tokens, Some(512));
        assert!(params.top_p.is_none());
        assert!(params.frequency_penalty.is_none());
        assert!(params.presence_penalty.is_none());
    }

    #[test]
    fn test_model_params_clone() {
        let params = ModelParams {
            temperature: Some(0.8),
            max_tokens: Some(2048),
            top_p: Some(0.95),
            frequency_penalty: Some(-0.5),
            presence_penalty: Some(0.3),
        };
        let cloned = params.clone();
        assert_eq!(params.temperature, cloned.temperature);
        assert_eq!(params.max_tokens, cloned.max_tokens);
        assert_eq!(params.top_p, cloned.top_p);
        assert_eq!(params.frequency_penalty, cloned.frequency_penalty);
        assert_eq!(params.presence_penalty, cloned.presence_penalty);
    }

    // ===== LLMResponse tests =====

    #[test]
    fn test_llm_response_empty_content() {
        let response = LLMResponse {
            content: String::new(),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
            usage: None,
        };
        assert!(response.content.is_empty());
    }

    #[test]
    fn test_llm_response_clone() {
        let response = LLMResponse {
            content: "hello".to_string(),
            tool_calls: vec![ToolCall {
                id: "1".to_string(),
                name: "fn".to_string(),
                arguments: serde_json::json!({"key": "value"}),
            }],
            finish_reason: "tool_calls".to_string(),
            usage: Some(TokenUsage::new(10, 20)),
        };
        let cloned = response.clone();
        assert_eq!(cloned.content, "hello");
        assert_eq!(cloned.tool_calls.len(), 1);
        assert_eq!(cloned.tool_calls[0].name, "fn");
        assert_eq!(cloned.finish_reason, "tool_calls");
        assert_eq!(cloned.usage.unwrap().total_tokens, 30);
    }

    // ===== OpenAI provider tests (feature-gated) =====

    #[cfg(feature = "openai")]
    mod openai_tests {
        use super::*;

        #[test]
        fn test_openai_name() {
            let provider = Provider::OpenAI {
                api_key: "sk-test".to_string(),
                api_base: "https://api.openai.com/v1".to_string(),
                model: "gpt-4".to_string(),
                params: ModelParams::default(),
            };
            assert_eq!(provider.name(), "openai");
        }

        #[test]
        fn test_openai_requires_api_key() {
            let provider = Provider::OpenAI {
                api_key: "sk-test".to_string(),
                api_base: "https://api.openai.com/v1".to_string(),
                model: "gpt-4".to_string(),
                params: ModelParams::default(),
            };
            assert!(provider.requires_api_key());
        }

        #[test]
        fn test_openai_is_local_localhost() {
            let provider = Provider::OpenAI {
                api_key: "test".to_string(),
                api_base: "http://localhost:8000/v1".to_string(),
                model: "local".to_string(),
                params: ModelParams::default(),
            };
            assert!(provider.is_local());
        }

        #[test]
        fn test_openai_is_local_127_0_0_1() {
            let provider = Provider::OpenAI {
                api_key: "test".to_string(),
                api_base: "http://127.0.0.1:8000/v1".to_string(),
                model: "local".to_string(),
                params: ModelParams::default(),
            };
            assert!(provider.is_local());
        }

        #[test]
        fn test_openai_is_not_local_remote() {
            let provider = Provider::OpenAI {
                api_key: "sk-test".to_string(),
                api_base: "https://api.openai.com/v1".to_string(),
                model: "gpt-4".to_string(),
                params: ModelParams::default(),
            };
            assert!(!provider.is_local());
        }

        #[test]
        fn test_openai_from_config_missing_env_var() {
            // Ensure the env var is not set to test the error path
            std::env::remove_var("TEST_OPENAI_MISSING_KEY");
            let config = ProviderConfig::OpenAI {
                api_key_env: "TEST_OPENAI_MISSING_KEY".to_string(),
                api_base: "https://api.openai.com/v1".to_string(),
                default_model: "gpt-4".to_string(),
            };
            let result = Provider::from_config(&config, None);
            assert!(result.is_err());
            match result.unwrap_err() {
                AppError::Configuration(msg) => {
                    assert!(msg.contains("TEST_OPENAI_MISSING_KEY"));
                }
                other => panic!("Expected Configuration error, got: {:?}", other),
            }
        }
    }

    #[test]
    fn test_token_usage_not_equal() {
        assert_ne!(TokenUsage::new(1, 2), TokenUsage::new(3, 4));
    }

    #[test]
    fn test_model_params_debug_format() {
        let params = ModelParams::default();
        let debug_str = format!("{:?}", params);
        assert!(debug_str.contains("ModelParams"));
    }

    fn test_stub_provider(model: &str) -> Provider {
        Provider::TestStub {
            model: model.to_string(),
        }
    }

    #[test]
    fn test_stub_provider_properties() {
        let provider = test_stub_provider("unit-test");
        assert_eq!(provider.name(), "test-stub");
        assert!(!provider.requires_api_key());
        assert!(provider.is_local());
    }

    #[tokio::test]
    async fn test_provider_create_client_test_stub() {
        let client = test_stub_provider("provider-model")
            .create_client()
            .await
            .expect("TestStub client");
        assert_eq!(client.model_name(), "provider-model");
    }

    #[tokio::test]
    async fn test_factory_create_default_via_test_stub() {
        let factory = LLMClientFactory::new(test_stub_provider("factory-model"));
        let client = factory.create_default().await.expect("factory client");
        assert_eq!(client.model_name(), "factory-model");
    }

    #[tokio::test]
    async fn test_factory_trait_create_with_provider() {
        let factory = LLMClientFactory::new(test_stub_provider("default"));
        let trait_ref: &dyn LLMClientFactoryTrait = &factory;
        let client = trait_ref
            .create_with_provider(test_stub_provider("switched"))
            .await
            .expect("switched client");
        assert_eq!(client.model_name(), "switched");
    }

    mod llm_client_trait_tests {
        use super::*;
        use crate::client::test_support::MockLLMClient;
        use crate::coordinator::{ConversationMessage, MessageRole};
        use ares_types::types::ToolDefinition;

        #[tokio::test]
        async fn test_generate_and_model_name() {
            let client = MockLLMClient::new("trait-model");
            assert_eq!(client.model_name(), "trait-model");
            let out = client.generate("hello").await.expect("generate");
            assert!(out.starts_with("mock-"));
        }

        #[tokio::test]
        async fn test_generate_with_system() {
            let client = MockLLMClient::new("sys");
            let out = client
                .generate_with_system("system", "prompt")
                .await
                .expect("generate_with_system");
            assert!(out.starts_with("mock-"));
        }

        #[tokio::test]
        async fn test_generate_with_history() {
            let client = MockLLMClient::new("hist");
            let messages = vec![("user".to_string(), "hi".to_string())];
            let response = client
                .generate_with_history(&messages)
                .await
                .expect("generate_with_history");
            assert!(response.content.starts_with("mock-"));
            assert_eq!(response.finish_reason, "stop");
            assert!(response.tool_calls.is_empty());
        }

        #[tokio::test]
        async fn test_generate_with_tools() {
            let client = MockLLMClient::new("tools");
            let tools = vec![ToolDefinition {
                name: "search".to_string(),
                description: "Search".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }];
            let response = client
                .generate_with_tools("find docs", &tools)
                .await
                .expect("generate_with_tools");
            assert!(response.content.starts_with("mock-"));
        }

        #[tokio::test]
        async fn test_generate_with_tools_and_history() {
            let client = MockLLMClient::new("both");
            let messages = vec![ConversationMessage {
                role: MessageRole::User,
                content: "run tool".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            }];
            let tools = vec![ToolDefinition {
                name: "calc".to_string(),
                description: "Calculate".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }];
            let response = client
                .generate_with_tools_and_history(&messages, &tools)
                .await
                .expect("generate_with_tools_and_history");
            assert!(response.content.starts_with("mock-"));
        }

        #[tokio::test]
        async fn test_stream_methods_return_internal_error() {
            let client = MockLLMClient::new("stream");
            for result in [
                client.stream("hi").await,
                client.stream_with_system("sys", "hi").await,
                client
                    .stream_with_history(&[("user".into(), "hi".into())])
                    .await,
            ] {
                assert!(matches!(result, Err(AppError::Internal(_))));
            }
        }
    }
}
