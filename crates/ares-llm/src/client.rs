use crate::config::{ModelConfig, ProviderConfig};
use ares_types::types::{AppError, Result, ToolCall, ToolDefinition};
use async_trait::async_trait;
use genai::adapter::AdapterKind;
use std::collections::HashMap;

/// Azure AI Foundry env/header helpers (no LLMClient). Inlined after
/// `azure.rs` was removed from this crate.
pub(crate) const AZURE_API_KEY_ENV: &str = "AZURE_FOUNDRY_API_KEY";
pub(crate) const AZURE_BASE_URL_ENV: &str = "AZURE_FOUNDRY_BASE_URL";
pub(crate) const AZURE_MODEL_ENV: &str = "AZURE_FOUNDRY_MODEL";
pub(crate) const AZURE_DEFAULT_MODEL: &str = "DeepSeek-V4-Flash";
const AZURE_MODEL_PREFIX: &str = "azure/";

pub(crate) fn azure_strip_model_prefix(model: &str) -> &str {
    let trimmed = model.trim();
    trimmed
        .strip_prefix(AZURE_MODEL_PREFIX)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(trimmed)
}

pub(crate) fn azure_normalize_base_url(api_base: &str) -> String {
    api_base.trim().trim_end_matches('/').to_string()
}

pub(crate) fn azure_foundry_headers(api_key: &str) -> HashMap<String, String> {
    let mut headers = HashMap::with_capacity(2);
    headers.insert("api-key".to_string(), api_key.to_string());
    headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    headers
}

/// Provider-neutral prompt cache policy mapped onto genai cache control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheControl {
    /// Default ephemeral cache.
    Ephemeral,
    /// Explicit 5-minute TTL.
    Ephemeral5m,
    /// Extended 24-hour TTL.
    Ephemeral24h,
}

/// Optional generation hints set on a client between calls.
///
/// Hints are OPT-IN: [`LLMClient::supports_hints`] defaults to `false` and
/// [`LLMClient::set_hints`] defaults to a no-op, so every existing provider
/// implementation keeps compiling and behaving exactly as before. A provider
/// adopts hints by overriding both methods (and honoring the stored hints in
/// its request building).
///
/// # Set-on-client semantics
///
/// `generate_with_system`'s signature is fixed and widely called, so hints
/// ride ON THE CLIENT instead of through per-call parameters: they apply to
/// all SUBSEQUENT generate calls until replaced by another `set_hints`
/// (clear with `GenerationHints::default()`).
///
/// # Thread safety
///
/// Implementations that adopt hints MUST use interior mutability (for
/// example `std::sync::RwLock<GenerationHints>`) because the trait methods
/// take `&self`. Readers snapshot the hints at the start of each call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GenerationHints {
    /// Ask the provider for a JSON object response.
    pub json_mode: bool,
    /// Ask a reasoning-capable model to skip visible reasoning output.
    pub suppress_reasoning: bool,
    /// Advisory maximum number of output tokens (`None` = provider default).
    pub max_tokens: Option<u32>,
    /// Optional constrained-output grammar in GBNF/EBNF-style syntax
    /// (`None` = unconstrained). Providers honor it only where a native
    /// mechanism exists; unsupported backends silently ignore it without
    /// erroring.
    pub guided_grammar: Option<String>,
    /// Reasoning effort keyword (`low|medium|high|zero`).
    pub reasoning_effort: Option<String>,
    /// OpenAI prompt cache key.
    pub prompt_cache_key: Option<String>,
    /// Request-level cache control.
    pub cache_control: Option<CacheControl>,
    /// Previous Responses API id for stateful continuation.
    pub previous_response_id: Option<String>,
    /// Whether the provider should store the response.
    pub store: Option<bool>,
    /// Attach the provider built-in web search tool (`provider_web_search`).
    pub web_search: bool,
}

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

    /// Whether this client honors [`GenerationHints`] set via
    /// [`LLMClient::set_hints`]. Defaults to `false`; hint-aware providers
    /// override this together with `set_hints`.
    fn supports_hints(&self) -> bool {
        false
    }

    /// Store generation hints applying to SUBSEQUENT generate calls, until
    /// replaced (clear with `GenerationHints::default()`). Default impl is a
    /// no-op so unmodified providers keep compiling unchanged.
    fn set_hints(&self, _hints: GenerationHints) {}

    /// Embed one or more input strings. Default: not supported.
    async fn embed(&self, _inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(AppError::FeatureDisabled(
            "embeddings not supported by this client".into(),
        ))
    }

    /// Whether this client can send image/file parts.
    fn supports_vision(&self) -> bool {
        false
    }

    /// Whether this client can attach the provider built-in web search tool.
    fn supports_provider_web_search(&self) -> bool {
        false
    }
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
    /// Tokens served from the provider-side prompt cache, when the provider
    /// reports cache hits (`None` when unknown or not reported). Always `0`
    /// or more; cache hits are a subset of `prompt_tokens`.
    #[serde(default)]
    pub cached_tokens: Option<i64>,
}

impl TokenUsage {
    /// Create a new TokenUsage with the given values
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            cached_tokens: None,
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
    /// Reasoning/thinking content when the model reports it.
    pub reasoning_content: Option<String>,
    /// Provider response id for stateful continuation (Responses API).
    pub response_id: Option<String>,
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

/// Resolved genai HTTP provider (kind + credentials + endpoint).
#[derive(Debug, Clone)]
pub struct GenaiProvider {
    /// genai adapter kind used for every call.
    pub kind: AdapterKind,
    /// API key (None for unauthenticated local adapters).
    pub api_key: Option<String>,
    /// Override endpoint; None uses the adapter default.
    pub endpoint: Option<String>,
    /// Model identifier.
    pub model: String,
    /// Sampling parameters.
    pub params: ModelParams,
    /// Extra HTTP headers (Azure Foundry, runtime providers).
    pub headers: HashMap<String, String>,
    /// AWS region (Bedrock API).
    pub region: Option<String>,
    /// GCP project (Vertex).
    pub vertex_project: Option<String>,
    /// Vertex location.
    pub vertex_location: Option<String>,
    /// Custom adapter index (`GENAI_{n}_*`).
    pub custom_index: Option<u8>,
}

impl GenaiProvider {
    fn openai(
        api_key: String,
        endpoint: String,
        model: String,
        params: ModelParams,
        headers: HashMap<String, String>,
    ) -> Self {
        Self {
            kind: AdapterKind::OpenAI,
            api_key: Some(api_key),
            endpoint: Some(endpoint),
            model,
            params,
            headers,
            region: None,
            vertex_project: None,
            vertex_location: None,
            custom_index: None,
        }
    }
}

/// LLM Provider configuration
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Provider {
    /// Any HTTP provider routed through genai.
    Genai(GenaiProvider),
    /// Local GGUF inference via llama.cpp.
    #[cfg(feature = "llamacpp")]
    LlamaCpp {
        /// Path to a GGUF model file.
        model_path: String,
        /// Model inference parameters.
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
    pub async fn create_client(&self) -> Result<Box<dyn LLMClient>> {
        match self {
            Provider::Genai(provider) => Ok(Box::new(crate::genai_client::GenaiClient::new(
                provider.clone(),
            )?)),
            #[cfg(feature = "llamacpp")]
            Provider::LlamaCpp { model_path, params } => Ok(Box::new(
                crate::llamacpp::LlamaCppClient::with_params(model_path.clone(), params.clone())?,
            )),
            #[cfg(test)]
            Provider::TestStub { model } => {
                Ok(Box::new(test_support::MockLLMClient::new(model.clone())))
            }
        }
    }

    /// Create a provider from environment variables.
    ///
    /// Priority: OPENAI_API_KEY, NVIDIA_API_KEY, AZURE_FOUNDRY_API_KEY,
    /// AWS_BEARER_TOKEN_BEDROCK, ANTHROPIC_API_KEY, GEMINI_API_KEY, else
    /// Ollama localhost.
    pub fn from_env() -> Result<Self> {
        if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
            if !api_key.is_empty() {
                let api_base = std::env::var("OPENAI_API_BASE")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into());
                let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4".into());
                return Ok(Provider::Genai(GenaiProvider::openai(
                    api_key,
                    api_base,
                    model,
                    ModelParams::default(),
                    HashMap::new(),
                )));
            }
        }

        if let Ok(api_key) = std::env::var("NVIDIA_API_KEY") {
            if !api_key.is_empty() {
                return Ok(Provider::Genai(GenaiProvider::openai(
                    api_key,
                    "https://integrate.api.nvidia.com/v1".into(),
                    "nvidia/nemotron-3-ultra-550b-a55b".into(),
                    ModelParams::default(),
                    HashMap::new(),
                )));
            }
        }

        if let Ok(api_key) = std::env::var(AZURE_API_KEY_ENV) {
            if !api_key.is_empty() {
                let api_base = std::env::var(AZURE_BASE_URL_ENV).map_err(|_| {
                    AppError::Configuration(format!(
                        "{} must be set when {} is configured",
                        AZURE_BASE_URL_ENV,
                        AZURE_API_KEY_ENV
                    ))
                })?;
                let model = std::env::var(AZURE_MODEL_ENV)
                    .unwrap_or_else(|_| AZURE_DEFAULT_MODEL.to_string());
                return Ok(Provider::Genai(GenaiProvider::openai(
                    api_key.clone(),
                    azure_normalize_base_url(&api_base),
                    azure_strip_model_prefix(&model).to_string(),
                    ModelParams::default(),
                    azure_foundry_headers(&api_key),
                )));
            }
        }

        if let Ok(api_key) = std::env::var("AWS_BEARER_TOKEN_BEDROCK") {
            if !api_key.is_empty() {
                let region = std::env::var("AWS_REGION").map_err(|_| {
                    AppError::Configuration(
                        "AWS_REGION must be set when AWS_BEARER_TOKEN_BEDROCK is configured".into(),
                    )
                })?;
                let model = std::env::var("BEDROCK_MODEL")
                    .unwrap_or_else(|_| "us.anthropic.claude-haiku-4-5-20251001-v1:0".into());
                return Ok(Provider::Genai(GenaiProvider {
                    kind: AdapterKind::BedrockApi,
                    api_key: Some(api_key),
                    endpoint: None,
                    model,
                    params: ModelParams::default(),
                    headers: HashMap::new(),
                    region: Some(region),
                    vertex_project: None,
                    vertex_location: None,
                    custom_index: None,
                }));
            }
        }

        if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
            if !api_key.is_empty() {
                let model = std::env::var("ANTHROPIC_MODEL")
                    .unwrap_or_else(|_| "claude-3-5-sonnet-20241022".into());
                return Ok(Provider::Genai(GenaiProvider {
                    kind: AdapterKind::Anthropic,
                    api_key: Some(api_key),
                    endpoint: None,
                    model,
                    params: ModelParams::default(),
                    headers: HashMap::new(),
                    region: None,
                    vertex_project: None,
                    vertex_location: None,
                    custom_index: None,
                }));
            }
        }

        if let Ok(api_key) = std::env::var("GEMINI_API_KEY") {
            if !api_key.is_empty() {
                let model =
                    std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.0-flash".into());
                return Ok(Provider::Genai(GenaiProvider {
                    kind: AdapterKind::Gemini,
                    api_key: Some(api_key),
                    endpoint: None,
                    model,
                    params: ModelParams::default(),
                    headers: HashMap::new(),
                    region: None,
                    vertex_project: None,
                    vertex_location: None,
                    custom_index: None,
                }));
            }
        }

        let base_url = std::env::var("OLLAMA_BASE_URL")
            .or_else(|_| std::env::var("OLLAMA_URL"))
            .unwrap_or_else(|_| "http://localhost:11434".into());
        let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "ministral-3:3b".into());
        Ok(Provider::Genai(GenaiProvider {
            kind: AdapterKind::Ollama,
            api_key: None,
            endpoint: Some(base_url),
            model,
            params: ModelParams::default(),
            headers: HashMap::new(),
            region: None,
            vertex_project: None,
            vertex_location: None,
            custom_index: None,
        }))
    }

    /// Get the provider name as a string
    pub fn name(&self) -> &'static str {
        match self {
            Provider::Genai(p) => p.kind.as_lower_str(),
            #[cfg(feature = "llamacpp")]
            Provider::LlamaCpp { .. } => "llamacpp",
            #[cfg(test)]
            Provider::TestStub { .. } => "test-stub",
        }
    }

    /// Check if this provider requires an API key
    pub fn requires_api_key(&self) -> bool {
        match self {
            Provider::Genai(p) => !matches!(p.kind, AdapterKind::Ollama),
            #[cfg(feature = "llamacpp")]
            Provider::LlamaCpp { .. } => false,
            #[cfg(test)]
            Provider::TestStub { .. } => false,
        }
    }

    /// Check if this provider is local (no network required)
    pub fn is_local(&self) -> bool {
        match self {
            Provider::Genai(p) => {
                if matches!(p.kind, AdapterKind::Ollama | AdapterKind::Omlx) {
                    return true;
                }
                p.endpoint
                    .as_deref()
                    .map(|u| u.contains("localhost") || u.contains("127.0.0.1"))
                    .unwrap_or(false)
            }
            #[cfg(feature = "llamacpp")]
            Provider::LlamaCpp { .. } => true,
            #[cfg(test)]
            Provider::TestStub { .. } => true,
        }
    }

    /// Create a provider from TOML configuration
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
        Ok(Provider::Genai(genai_from_config(
            provider_config,
            model_override,
            params,
        )?))
    }

    /// Create a provider from a model configuration and its associated provider config
    pub fn from_model_config(
        model_config: &ModelConfig,
        provider_config: &ProviderConfig,
    ) -> Result<Self> {
        let params = ModelParams::from_model_config(model_config);
        Self::from_config_with_params(provider_config, Some(&model_config.model), params)
    }

    /// Create a runtime OpenAI-compatible provider from a runtime provider entry.
    pub fn from_runtime_openai(
        api_key: String,
        api_base: String,
        model: String,
        params: ModelParams,
        headers: HashMap<String, String>,
    ) -> Self {
        Provider::Genai(GenaiProvider::openai(
            api_key, api_base, model, params, headers,
        ))
    }

    /// Create a runtime Bedrock provider from a runtime provider entry.
    pub fn from_runtime_bedrock(
        api_key: String,
        region: String,
        model: String,
        params: ModelParams,
    ) -> Self {
        Provider::Genai(GenaiProvider {
            kind: AdapterKind::BedrockApi,
            api_key: Some(api_key),
            endpoint: None,
            model,
            params,
            headers: HashMap::new(),
            region: Some(region),
            vertex_project: None,
            vertex_location: None,
            custom_index: None,
        })
    }
}

fn require_env(name: &str, what: &str) -> Result<String> {
    std::env::var(name).map_err(|_| {
        AppError::Configuration(format!(
            "{what} environment variable '{name}' is not set"
        ))
    })
}

fn pick_model(model_override: Option<&str>, default_model: &str) -> String {
    model_override
        .map(String::from)
        .unwrap_or_else(|| default_model.to_string())
}

fn genai_from_config(
    config: &ProviderConfig,
    model_override: Option<&str>,
    params: ModelParams,
) -> Result<GenaiProvider> {
    match config {
        ProviderConfig::OpenAI {
            api_key_env,
            api_base,
            default_model,
        } => {
            let api_key = require_env(api_key_env, "OpenAI API key")?;
            Ok(GenaiProvider::openai(
                api_key,
                api_base.clone(),
                pick_model(model_override, default_model),
                params,
                HashMap::new(),
            ))
        }
        ProviderConfig::Azure {
            api_key_env,
            base_url_env,
            default_model,
        } => {
            let api_key = require_env(api_key_env, "Azure Foundry API key")?;
            let api_base = require_env(base_url_env, "Azure Foundry base URL")?;
            Ok(GenaiProvider::openai(
                api_key.clone(),
                azure_normalize_base_url(&api_base),
                azure_strip_model_prefix(&pick_model(model_override, default_model))
                    .to_string(),
                params,
                azure_foundry_headers(&api_key),
            ))
        }
        ProviderConfig::Anthropic {
            api_key_env,
            default_model,
        } => {
            let api_key = require_env(api_key_env, "Anthropic API key")?;
            Ok(GenaiProvider {
                kind: AdapterKind::Anthropic,
                api_key: Some(api_key),
                endpoint: None,
                model: pick_model(model_override, default_model),
                params,
                headers: HashMap::new(),
                region: None,
                vertex_project: None,
                vertex_location: None,
                custom_index: None,
            })
        }
        ProviderConfig::Bedrock {
            api_key_env,
            region_env,
            default_model,
        } => {
            let api_key = require_env(api_key_env, "Bedrock API key")?;
            let region = require_env(region_env, "Bedrock region")?;
            Ok(GenaiProvider {
                kind: AdapterKind::BedrockApi,
                api_key: Some(api_key),
                endpoint: None,
                model: pick_model(model_override, default_model),
                params,
                headers: HashMap::new(),
                region: Some(region),
                vertex_project: None,
                vertex_location: None,
                custom_index: None,
            })
        }
        ProviderConfig::Ollama {
            base_url,
            default_model,
            ..
        } => Ok(GenaiProvider {
            kind: AdapterKind::Ollama,
            api_key: None,
            endpoint: Some(base_url.clone()),
            model: pick_model(model_override, default_model),
            params,
            headers: HashMap::new(),
            region: None,
            vertex_project: None,
            vertex_location: None,
            custom_index: None,
        }),
        ProviderConfig::Vertex {
            api_key_env,
            project_env,
            location_env,
            default_model,
        } => {
            let api_key = require_env(api_key_env, "Vertex API key")?;
            let project = std::env::var(project_env).ok();
            let location = std::env::var(location_env).ok();
            Ok(GenaiProvider {
                kind: AdapterKind::Vertex,
                api_key: Some(api_key),
                endpoint: None,
                model: pick_model(model_override, default_model),
                params,
                headers: HashMap::new(),
                region: None,
                vertex_project: project,
                vertex_location: location,
                custom_index: None,
            })
        }
        ProviderConfig::Custom {
            index,
            endpoint,
            api_key_env,
            default_model,
        } => {
            let api_key = match api_key_env {
                Some(env) if !env.is_empty() => Some(require_env(env, "Custom API key")?),
                _ => std::env::var(format!("GENAI_{index}_API_KEY")).ok(),
            };
            Ok(GenaiProvider {
                kind: AdapterKind::Custom(*index),
                api_key,
                endpoint: Some(endpoint.clone()),
                model: pick_model(model_override, default_model),
                params,
                headers: HashMap::new(),
                region: None,
                vertex_project: None,
                vertex_location: None,
                custom_index: Some(*index),
            })
        }
        other => {
            let (kind, api_key_env, api_base, default_model) = simple_genai_fields(other);
            let optional_key = matches!(kind, AdapterKind::Omlx);
            let api_key = if optional_key {
                std::env::var(api_key_env).ok().filter(|s| !s.is_empty())
            } else {
                Some(require_env(
                    api_key_env,
                    &format!("{} API key", other.type_name()),
                )?)
            };
            Ok(GenaiProvider {
                kind,
                api_key,
                endpoint: api_base.filter(|s| !s.is_empty()),
                model: pick_model(model_override, default_model),
                params,
                headers: HashMap::new(),
                region: None,
                vertex_project: None,
                vertex_location: None,
                custom_index: None,
            })
        }
    }
}

fn simple_genai_fields(
    config: &ProviderConfig,
) -> (AdapterKind, &str, Option<String>, &str) {
    match config {
        ProviderConfig::OpenAIResp {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::OpenAIResp,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Gemini {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::Gemini,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Fireworks {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::Fireworks,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Together {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::Together,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Groq {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::Groq,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Aihubmix {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::Aihubmix,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Kimi {
            api_key_env,
            api_base,
            default_model,
        } => (AdapterKind::Kimi, api_key_env, api_base.clone(), default_model),
        ProviderConfig::Mimo {
            api_key_env,
            api_base,
            default_model,
        } => (AdapterKind::Mimo, api_key_env, api_base.clone(), default_model),
        ProviderConfig::Moonshot {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::Moonshot,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Nebius {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::Nebius,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Xai {
            api_key_env,
            api_base,
            default_model,
        } => (AdapterKind::Xai, api_key_env, api_base.clone(), default_model),
        ProviderConfig::DeepSeek {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::DeepSeek,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Zai {
            api_key_env,
            api_base,
            default_model,
        } => (AdapterKind::Zai, api_key_env, api_base.clone(), default_model),
        ProviderConfig::BigModel {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::BigModel,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Aliyun {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::Aliyun,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::QwenCloud {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::QwenCloud,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Baidu {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::Baidu,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Cohere {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::Cohere,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::OllamaCloud {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::OllamaCloud,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::Omlx {
            api_key_env,
            api_base,
            default_model,
        } => (AdapterKind::Omlx, api_key_env, api_base.clone(), default_model),
        ProviderConfig::GithubCopilot {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::GithubCopilot,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::OpenCodeGo {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::OpenCodeGo,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::BedrockApi {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::BedrockApi,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::OpenRouter {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::OpenRouter,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::AtlasCloud {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::AtlasCloud,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        ProviderConfig::MiniMax {
            api_key_env,
            api_base,
            default_model,
        } => (
            AdapterKind::MiniMax,
            api_key_env,
            api_base.clone(),
            default_model,
        ),
        other => unreachable!("simple_genai_fields on {}", other.type_name()),
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
pub struct LLMClientFactory {
    default_provider: Provider,
}

impl LLMClientFactory {
    /// Create a new factory with a specific default provider
    pub fn new(default_provider: Provider) -> Self {
        Self { default_provider }
    }

    /// Create a factory from environment variables
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
                reasoning_content: None,
                response_id: None,
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
                reasoning_content: None,
                response_id: None,
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
                reasoning_content: None,
                response_id: None,
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

    fn llm_text(content: impl Into<String>) -> LLMResponse {
        LLMResponse {
            content: content.into(),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
            usage: None,
            reasoning_content: None,
            response_id: None,
        }
    }

    fn genai_openai(api_base: &str, model: &str) -> Provider {
        Provider::Genai(GenaiProvider::openai(
            "sk-test".into(),
            api_base.into(),
            model.into(),
            ModelParams::default(),
            HashMap::new(),
        ))
    }

    #[test]
    fn test_llm_response_creation() {
        let response = llm_text("Hello");
        assert_eq!(response.content, "Hello");
        assert!(response.tool_calls.is_empty());
        assert_eq!(response.finish_reason, "stop");
        assert!(response.usage.is_none());
        assert!(response.reasoning_content.is_none());
        assert!(response.response_id.is_none());
    }

    #[test]
    fn test_llm_response_with_usage() {
        let usage = TokenUsage::new(100, 50);
        let response = LLMResponse {
            content: "Hello".to_string(),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
            usage: Some(usage),
            reasoning_content: None,
            response_id: None,
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
            reasoning_content: None,
            response_id: None,
        };

        assert_eq!(response.tool_calls.len(), 2);
        assert_eq!(response.tool_calls[0].name, "calculator");
        assert_eq!(response.finish_reason, "tool_calls");
        assert_eq!(response.usage.as_ref().unwrap().total_tokens, 75);
    }

    #[test]
    fn test_factory_creation() {
        let factory = LLMClientFactory::new(genai_openai("https://api.openai.com/v1", "test"));
        assert_eq!(factory.default_provider().name(), "openai");
    }

    #[test]
    fn test_openai_provider_properties() {
        let provider = genai_openai("https://api.openai.com/v1", "gpt-4");
        assert_eq!(provider.name(), "openai");
        assert!(provider.requires_api_key());
        assert!(!provider.is_local());
    }

    #[test]
    fn test_openai_local_provider() {
        let provider = genai_openai("http://localhost:8000/v1", "local-model");
        assert!(provider.is_local());
    }

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

    #[test]
    fn test_llm_response_empty_content() {
        let response = llm_text("");
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
            reasoning_content: None,
            response_id: None,
        };
        let cloned = response.clone();
        assert_eq!(cloned.content, "hello");
        assert_eq!(cloned.tool_calls.len(), 1);
        assert_eq!(cloned.tool_calls[0].name, "fn");
        assert_eq!(cloned.finish_reason, "tool_calls");
        assert_eq!(cloned.usage.unwrap().total_tokens, 30);
    }

    #[test]
    fn test_openai_from_config_missing_env_var() {
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
        use crate::coordinator::ConversationMessage;
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
            let messages = vec![ConversationMessage::user("run tool")];
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

        #[test]
        fn default_supports_hints_is_false() {
            let client = MockLLMClient::new("hints");
            assert!(!client.supports_hints());
            client.set_hints(GenerationHints {
                json_mode: true,
                ..Default::default()
            });
        }

        #[test]
        fn hint_recording_mock_records_set_hints_calls() {
            use parking_lot::Mutex;
            use std::sync::Arc;

            #[derive(Default)]
            struct HintRecordingClient {
                hints: Mutex<Vec<GenerationHints>>,
            }

            #[async_trait]
            impl LLMClient for HintRecordingClient {
                async fn generate(&self, _prompt: &str) -> Result<String> {
                    Err(AppError::Internal("unused".into()))
                }

                async fn generate_with_system(
                    &self,
                    _system: &str,
                    _prompt: &str,
                ) -> Result<String> {
                    Err(AppError::Internal("unused".into()))
                }

                async fn generate_with_history(
                    &self,
                    _messages: &[(String, String)],
                ) -> Result<LLMResponse> {
                    Err(AppError::Internal("unused".into()))
                }

                async fn generate_with_tools(
                    &self,
                    _prompt: &str,
                    _tools: &[ToolDefinition],
                ) -> Result<LLMResponse> {
                    Err(AppError::Internal("unused".into()))
                }

                async fn generate_with_tools_and_history(
                    &self,
                    _messages: &[crate::coordinator::ConversationMessage],
                    _tools: &[ToolDefinition],
                ) -> Result<LLMResponse> {
                    Err(AppError::Internal("unused".into()))
                }

                async fn stream(
                    &self,
                    _prompt: &str,
                ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
                {
                    Err(AppError::Internal("unused".into()))
                }

                async fn stream_with_system(
                    &self,
                    _system: &str,
                    _prompt: &str,
                ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
                {
                    Err(AppError::Internal("unused".into()))
                }

                async fn stream_with_history(
                    &self,
                    _messages: &[(String, String)],
                ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>>
                {
                    Err(AppError::Internal("unused".into()))
                }

                fn model_name(&self) -> &str {
                    "hint-recording-mock"
                }

                fn supports_hints(&self) -> bool {
                    true
                }

                fn set_hints(&self, hints: GenerationHints) {
                    self.hints.lock().push(hints);
                }
            }

            let client = Arc::new(HintRecordingClient::default());
            assert!(client.supports_hints());
            client.set_hints(GenerationHints {
                json_mode: true,
                suppress_reasoning: false,
                max_tokens: Some(256),
                guided_grammar: None,
                ..Default::default()
            });
            client.set_hints(GenerationHints::default());

            let recorded = client.hints.lock();
            assert_eq!(recorded.len(), 2, "every set_hints call is recorded in order");
            assert!(recorded[0].json_mode && recorded[0].max_tokens == Some(256));
            assert_eq!(
                recorded[1],
                GenerationHints::default(),
                "clearing via Default::default() reaches the impl"
            );
        }
    }
}
