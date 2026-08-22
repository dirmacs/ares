use serde::{Deserialize, Serialize};

// ============= Provider Configuration =============

/// LLM provider configuration.
///
/// `openai` covers the OpenAI API and OpenAI-compatible endpoints
/// (NVIDIA NIM, Groq, Azure OpenAI, local vLLM, etc.).
///
/// `azure` covers Azure AI Foundry's OpenAI-compatible `/openai/v1`
/// endpoints while reading both the API key and base URL from environment
/// variables.
///
/// `bedrock` requires the `bedrock` feature on `ares-llm`. `anthropic`
/// requires the `anthropic` feature on `ares-llm`. `ollama`
/// requires the `ollama` feature. These are compiled out of the default
/// production build (`--no-default-features --features openai,postgres,mcp`),
/// so they are not present in `ares-server` binaries that omit those
/// features. The `ares.toml` schema is the same regardless: the runtime
/// `Provider::from_config_with_params` returns a clear configuration error
/// when a variant is selected without the corresponding feature enabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[non_exhaustive]
pub enum ProviderConfig {
    /// OpenAI API (or compatible endpoints, including NVIDIA NIM).
    OpenAI {
        /// Environment variable containing API key.
        api_key_env: String,
        /// API base URL (default: `https://api.openai.com/v1`).
        #[serde(default = "default_openai_base")]
        api_base: String,
        /// Default model to use with this provider.
        default_model: String,
    },
    /// Azure AI Foundry OpenAI-compatible chat completions.
    Azure {
        /// Environment variable containing the Foundry API key.
        #[serde(default = "default_azure_api_key_env")]
        api_key_env: String,
        /// Environment variable containing the Foundry base URL.
        #[serde(default = "default_azure_base_url_env")]
        base_url_env: String,
        /// Default Foundry model id.
        #[serde(default = "default_azure_default_model")]
        default_model: String,
    },
    /// Anthropic Claude API.
    Anthropic {
        /// Environment variable containing API key (default: `ANTHROPIC_API_KEY`).
        #[serde(default = "default_anthropic_api_key_env")]
        api_key_env: String,
        /// Default model (default: `claude-3-5-sonnet-20241022`).
        #[serde(default = "default_anthropic_default_model")]
        default_model: String,
    },
    /// AWS Bedrock Claude API.
    Bedrock {
        /// Environment variable containing the Bedrock bearer token.
        #[serde(default = "default_bedrock_api_key_env")]
        api_key_env: String,
        /// Environment variable containing the AWS region.
        #[serde(default = "default_bedrock_region_env")]
        region_env: String,
        /// Default Bedrock model id.
        #[serde(default = "default_bedrock_default_model")]
        default_model: String,
    },
    /// Local Ollama server. The `api_key_env` is a dummy field (Ollama does
    /// not require authentication) so the same fleet-secrets storage layer can
    /// be used uniformly for all providers.
    Ollama {
        /// Dummy env var; unused at runtime.
        #[serde(default = "default_ollama_api_key_env")]
        api_key_env: String,
        /// Base URL of the Ollama server.
        #[serde(default = "default_ollama_base_url")]
        base_url: String,
        /// Default model id (e.g. `ministral-3:3b`).
        #[serde(default = "default_ollama_default_model")]
        default_model: String,
    },
}

impl ProviderConfig {
    /// Returns the provider type discriminator.
    pub fn type_name(&self) -> &'static str {
        match self {
            ProviderConfig::OpenAI { .. } => "openai",
            ProviderConfig::Azure { .. } => "azure",
            ProviderConfig::Anthropic { .. } => "anthropic",
            ProviderConfig::Bedrock { .. } => "bedrock",
            ProviderConfig::Ollama { .. } => "ollama",
        }
    }
}

impl std::str::FromStr for ProviderConfig {
    type Err = String;

    /// Parse a provider type name into a default `ProviderConfig` variant.
    /// Accepts `openai` (and the `nvidia` alias), `azure`, `anthropic`, `bedrock`, and `ollama`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "openai" | "nvidia" => Ok(ProviderConfig::OpenAI {
                api_key_env: default_nvidia_api_key_env(),
                api_base: default_nvidia_api_base(),
                default_model: default_nvidia_default_model(),
            }),
            "azure" => Ok(ProviderConfig::Azure {
                api_key_env: default_azure_api_key_env(),
                base_url_env: default_azure_base_url_env(),
                default_model: default_azure_default_model(),
            }),
            "anthropic" => Ok(ProviderConfig::Anthropic {
                api_key_env: default_anthropic_api_key_env(),
                default_model: default_anthropic_default_model(),
            }),
            "bedrock" => Ok(ProviderConfig::Bedrock {
                api_key_env: default_bedrock_api_key_env(),
                region_env: default_bedrock_region_env(),
                default_model: default_bedrock_default_model(),
            }),
            "ollama" => Ok(ProviderConfig::Ollama {
                api_key_env: default_ollama_api_key_env(),
                base_url: default_ollama_base_url(),
                default_model: default_ollama_default_model(),
            }),
            other => Err(format!(
                "Unknown provider type: {other}. Use: openai (or nvidia), azure, anthropic, bedrock, ollama"
            )),
        }
    }
}

fn default_openai_base() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_nvidia_api_key_env() -> String {
    "NVIDIA_API_KEY".to_string()
}

fn default_nvidia_api_base() -> String {
    "https://integrate.api.nvidia.com/v1".to_string()
}

fn default_nvidia_default_model() -> String {
    "nvidia/nemotron-3-ultra-550b-a55b".to_string()
}

fn default_azure_api_key_env() -> String {
    "AZURE_FOUNDRY_API_KEY".to_string()
}

fn default_azure_base_url_env() -> String {
    "AZURE_FOUNDRY_BASE_URL".to_string()
}

fn default_azure_default_model() -> String {
    "DeepSeek-V4-Flash".to_string()
}

fn default_anthropic_api_key_env() -> String {
    "ANTHROPIC_API_KEY".to_string()
}

fn default_anthropic_default_model() -> String {
    "claude-3-5-sonnet-20241022".to_string()
}

fn default_bedrock_api_key_env() -> String {
    "AWS_BEARER_TOKEN_BEDROCK".to_string()
}

fn default_bedrock_region_env() -> String {
    "AWS_REGION".to_string()
}

fn default_bedrock_default_model() -> String {
    "us.anthropic.claude-haiku-4-5-20251001-v1:0".to_string()
}

fn default_ollama_api_key_env() -> String {
    "OLLAMA_API_KEY".to_string()
}

fn default_ollama_base_url() -> String {
    "http://localhost:11434".to_string()
}

fn default_ollama_default_model() -> String {
    "ministral-3:3b".to_string()
}

// ============= Model Configuration =============

/// Model configuration referencing a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Reference to a provider name defined in \[providers\].
    pub provider: String,

    /// Model name/identifier to use with the provider.
    pub model: String,

    /// Sampling temperature (0.0 = deterministic, 1.0+ = creative). Default: 0.7.
    #[serde(default = "default_temperature")]
    pub temperature: f32,

    /// Maximum tokens to generate (default: 512).
    #[serde(default = "default_model_max_tokens")]
    pub max_tokens: u32,
}

fn default_temperature() -> f32 {
    0.7
}

fn default_model_max_tokens() -> u32 {
    512
}
