use serde::{Deserialize, Serialize};

// ============= Provider Configuration =============

/// LLM provider configuration.
///
/// Internally tagged with `type`. Existing `openai` / `azure` / `anthropic` /
/// `bedrock` / `ollama` shapes are unchanged. Additional variants match genai
/// `AdapterKind::as_lower_str` names. `type = "bedrock"` remains ARES Bedrock
/// (Bearer via `AWS_BEARER_TOKEN_BEDROCK`); genai's Bedrock API kind is
/// `bedrock_api`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum ProviderConfig {
    /// OpenAI API (or compatible endpoints, including NVIDIA NIM).
    #[serde(rename = "openai")]
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
    #[serde(rename = "azure")]
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
    #[serde(rename = "anthropic")]
    Anthropic {
        /// Environment variable containing API key (default: `ANTHROPIC_API_KEY`).
        #[serde(default = "default_anthropic_api_key_env")]
        api_key_env: String,
        /// Default model (default: `claude-3-5-sonnet-20241022`).
        #[serde(default = "default_anthropic_default_model")]
        default_model: String,
    },
    /// AWS Bedrock Claude API (ARES bearer token).
    #[serde(rename = "bedrock")]
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
    /// Local Ollama server.
    #[serde(rename = "ollama")]
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
    /// Google Vertex AI.
    #[serde(rename = "vertex")]
    Vertex {
        /// Environment variable containing the Vertex API key.
        #[serde(default = "default_vertex_api_key_env")]
        api_key_env: String,
        /// Environment variable containing the GCP project id.
        #[serde(default = "default_vertex_project_env")]
        project_env: String,
        /// Environment variable containing the Vertex location.
        #[serde(default = "default_vertex_location_env")]
        location_env: String,
        /// Default model.
        #[serde(default)]
        default_model: String,
    },
    /// Bring-your-own OpenAI-compatible endpoint (`GENAI_{n}_*`).
    #[serde(rename = "custom")]
    Custom {
        /// Custom adapter index (`GENAI_{index}_ENDPOINT`).
        index: u8,
        /// Endpoint URL.
        endpoint: String,
        /// Optional environment variable containing an API key.
        #[serde(default)]
        api_key_env: Option<String>,
        /// Default model.
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "openai_resp")]
    OpenAIResp {
        #[serde(default = "default_openai_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "gemini")]
    Gemini {
        #[serde(default = "default_gemini_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "fireworks")]
    Fireworks {
        #[serde(default = "default_fireworks_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "together")]
    Together {
        #[serde(default = "default_together_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "groq")]
    Groq {
        #[serde(default = "default_groq_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "aihubmix")]
    Aihubmix {
        #[serde(default = "default_aihubmix_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "kimi")]
    Kimi {
        #[serde(default = "default_kimi_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "mimo")]
    Mimo {
        #[serde(default = "default_mimo_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "moonshot")]
    Moonshot {
        #[serde(default = "default_moonshot_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "nebius")]
    Nebius {
        #[serde(default = "default_nebius_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "xai")]
    Xai {
        #[serde(default = "default_xai_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "deepseek")]
    DeepSeek {
        #[serde(default = "default_deepseek_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "zai")]
    Zai {
        #[serde(default = "default_zai_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "bigmodel")]
    BigModel {
        #[serde(default = "default_bigmodel_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "aliyun")]
    Aliyun {
        #[serde(default = "default_aliyun_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "qwen_cloud")]
    QwenCloud {
        #[serde(default = "default_qwen_cloud_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "baidu")]
    Baidu {
        #[serde(default = "default_baidu_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "cohere")]
    Cohere {
        #[serde(default = "default_cohere_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "ollama_cloud")]
    OllamaCloud {
        #[serde(default = "default_ollama_cloud_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "omlx")]
    Omlx {
        #[serde(default = "default_omlx_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "github_copilot", alias = "github")]
    GithubCopilot {
        #[serde(default = "default_github_copilot_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "opencode_go")]
    OpenCodeGo {
        #[serde(default = "default_opencode_go_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "bedrock_api")]
    BedrockApi {
        #[serde(default = "default_bedrock_api_kind_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "open_router", alias = "openrouter")]
    OpenRouter {
        #[serde(default = "default_open_router_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "atlascloud")]
    AtlasCloud {
        #[serde(default = "default_atlascloud_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
    #[serde(rename = "minimax")]
    MiniMax {
        #[serde(default = "default_minimax_api_key_env")]
        api_key_env: String,
        #[serde(default)]
        api_base: Option<String>,
        #[serde(default)]
        default_model: String,
    },
}

macro_rules! type_name_match {
    ($self:expr, $($variant:ident => $name:literal),* $(,)?) => {
        match $self {
            $(ProviderConfig::$variant { .. } => $name,)*
        }
    };
}

impl ProviderConfig {
    /// Returns the provider type discriminator (genai lower string / ARES type).
    pub fn type_name(&self) -> &'static str {
        type_name_match!(
            self,
            OpenAI => "openai",
            Azure => "azure",
            Anthropic => "anthropic",
            Bedrock => "bedrock",
            Ollama => "ollama",
            Vertex => "vertex",
            Custom => "custom",
            OpenAIResp => "openai_resp",
            Gemini => "gemini",
            Fireworks => "fireworks",
            Together => "together",
            Groq => "groq",
            Aihubmix => "aihubmix",
            Kimi => "kimi",
            Mimo => "mimo",
            Moonshot => "moonshot",
            Nebius => "nebius",
            Xai => "xai",
            DeepSeek => "deepseek",
            Zai => "zai",
            BigModel => "bigmodel",
            Aliyun => "aliyun",
            QwenCloud => "qwen_cloud",
            Baidu => "baidu",
            Cohere => "cohere",
            OllamaCloud => "ollama_cloud",
            Omlx => "omlx",
            GithubCopilot => "github_copilot",
            OpenCodeGo => "opencode_go",
            BedrockApi => "bedrock_api",
            OpenRouter => "open_router",
            AtlasCloud => "atlascloud",
            MiniMax => "minimax",
        )
    }

    /// Default model id configured for this provider.
    pub fn default_model(&self) -> &str {
        match self {
            ProviderConfig::OpenAI { default_model, .. }
            | ProviderConfig::Azure { default_model, .. }
            | ProviderConfig::Anthropic { default_model, .. }
            | ProviderConfig::Bedrock { default_model, .. }
            | ProviderConfig::Ollama { default_model, .. }
            | ProviderConfig::Vertex { default_model, .. }
            | ProviderConfig::Custom { default_model, .. }
            | ProviderConfig::OpenAIResp { default_model, .. }
            | ProviderConfig::Gemini { default_model, .. }
            | ProviderConfig::Fireworks { default_model, .. }
            | ProviderConfig::Together { default_model, .. }
            | ProviderConfig::Groq { default_model, .. }
            | ProviderConfig::Aihubmix { default_model, .. }
            | ProviderConfig::Kimi { default_model, .. }
            | ProviderConfig::Mimo { default_model, .. }
            | ProviderConfig::Moonshot { default_model, .. }
            | ProviderConfig::Nebius { default_model, .. }
            | ProviderConfig::Xai { default_model, .. }
            | ProviderConfig::DeepSeek { default_model, .. }
            | ProviderConfig::Zai { default_model, .. }
            | ProviderConfig::BigModel { default_model, .. }
            | ProviderConfig::Aliyun { default_model, .. }
            | ProviderConfig::QwenCloud { default_model, .. }
            | ProviderConfig::Baidu { default_model, .. }
            | ProviderConfig::Cohere { default_model, .. }
            | ProviderConfig::OllamaCloud { default_model, .. }
            | ProviderConfig::Omlx { default_model, .. }
            | ProviderConfig::GithubCopilot { default_model, .. }
            | ProviderConfig::OpenCodeGo { default_model, .. }
            | ProviderConfig::BedrockApi { default_model, .. }
            | ProviderConfig::OpenRouter { default_model, .. }
            | ProviderConfig::AtlasCloud { default_model, .. }
            | ProviderConfig::MiniMax { default_model, .. } => default_model,
        }
    }
}

impl std::str::FromStr for ProviderConfig {
    type Err = String;

    /// Parse a provider type name into a default `ProviderConfig` variant.
    /// `nvidia` stays an OpenAI-compat alias at the NIM base.
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let key = s.trim().to_lowercase();
        let key = match key.as_str() {
            "openrouter" => "open_router",
            "github" => "github_copilot",
            other => other,
        };
        match key {
            "openai" => Ok(ProviderConfig::OpenAI {
                api_key_env: default_openai_api_key_env(),
                api_base: default_openai_base(),
                default_model: "gpt-4".to_string(),
            }),
            "nvidia" => Ok(ProviderConfig::OpenAI {
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
            "vertex" => Ok(ProviderConfig::Vertex {
                api_key_env: default_vertex_api_key_env(),
                project_env: default_vertex_project_env(),
                location_env: default_vertex_location_env(),
                default_model: String::new(),
            }),
            "custom" => Ok(ProviderConfig::Custom {
                index: 1,
                endpoint: String::new(),
                api_key_env: None,
                default_model: String::new(),
            }),
            other => {
                let json = format!(r#"{{"type":"{other}"}}"#);
                serde_json::from_str(&json).map_err(|_| {
                    format!(
                        "Unknown provider type: {other}. Use: openai (or nvidia), azure, anthropic, bedrock, ollama, or a genai adapter kind"
                    )
                })
            }
        }
    }
}

fn default_openai_base() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_openai_api_key_env() -> String {
    "OPENAI_API_KEY".to_string()
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

fn default_vertex_api_key_env() -> String {
    "VERTEX_API_KEY".to_string()
}

fn default_vertex_project_env() -> String {
    "VERTEX_PROJECT_ID".to_string()
}

fn default_vertex_location_env() -> String {
    "VERTEX_LOCATION".to_string()
}

fn default_gemini_api_key_env() -> String {
    "GEMINI_API_KEY".to_string()
}
fn default_fireworks_api_key_env() -> String {
    "FIREWORKS_API_KEY".to_string()
}
fn default_together_api_key_env() -> String {
    "TOGETHER_API_KEY".to_string()
}
fn default_groq_api_key_env() -> String {
    "GROQ_API_KEY".to_string()
}
fn default_aihubmix_api_key_env() -> String {
    "AIHUBMIX_API_KEY".to_string()
}
fn default_kimi_api_key_env() -> String {
    "KIMI_API_KEY".to_string()
}
fn default_mimo_api_key_env() -> String {
    "MIMO_API_KEY".to_string()
}
fn default_moonshot_api_key_env() -> String {
    "MOONSHOT_API_KEY".to_string()
}
fn default_nebius_api_key_env() -> String {
    "NEBIUS_API_KEY".to_string()
}
fn default_xai_api_key_env() -> String {
    "XAI_API_KEY".to_string()
}
fn default_deepseek_api_key_env() -> String {
    "DEEPSEEK_API_KEY".to_string()
}
fn default_zai_api_key_env() -> String {
    "ZAI_API_KEY".to_string()
}
fn default_bigmodel_api_key_env() -> String {
    "BIGMODEL_API_KEY".to_string()
}
fn default_aliyun_api_key_env() -> String {
    "ALIYUN_API_KEY".to_string()
}
fn default_qwen_cloud_api_key_env() -> String {
    "QWEN_CLOUD_API_KEY".to_string()
}
fn default_baidu_api_key_env() -> String {
    "BAIDU_API_KEY".to_string()
}
fn default_cohere_api_key_env() -> String {
    "COHERE_API_KEY".to_string()
}
fn default_ollama_cloud_api_key_env() -> String {
    "OLLAMA_API_KEY".to_string()
}
fn default_omlx_api_key_env() -> String {
    "OMLX_API_KEY".to_string()
}
fn default_github_copilot_api_key_env() -> String {
    "GITHUB_TOKEN".to_string()
}
fn default_opencode_go_api_key_env() -> String {
    "OPENCODE_GO_API_KEY".to_string()
}
fn default_bedrock_api_kind_api_key_env() -> String {
    "BEDROCK_API_KEY".to_string()
}
fn default_open_router_api_key_env() -> String {
    "OPEN_ROUTER_API_KEY".to_string()
}
fn default_atlascloud_api_key_env() -> String {
    "ATLASCLOUD_API_KEY".to_string()
}
fn default_minimax_api_key_env() -> String {
    "MINIMAX_API_KEY".to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn parses_gemini_groq_openai_resp_custom() {
        let gemini: ProviderConfig = serde_json::from_str(r#"{"type":"gemini"}"#).unwrap();
        assert_eq!(gemini.type_name(), "gemini");
        assert!(matches!(gemini, ProviderConfig::Gemini { .. }));

        let groq: ProviderConfig = serde_json::from_str(r#"{"type":"groq"}"#).unwrap();
        assert_eq!(groq.type_name(), "groq");

        let resp: ProviderConfig = serde_json::from_str(r#"{"type":"openai_resp"}"#).unwrap();
        assert_eq!(resp.type_name(), "openai_resp");

        let custom: ProviderConfig = serde_json::from_str(
            r#"{"type":"custom","index":2,"endpoint":"http://127.0.0.1:8000/v1"}"#,
        )
        .unwrap();
        match custom {
            ProviderConfig::Custom {
                index, endpoint, ..
            } => {
                assert_eq!(index, 2);
                assert_eq!(endpoint, "http://127.0.0.1:8000/v1");
            }
            other => panic!("expected Custom, got {}", other.type_name()),
        }

        assert_eq!(
            ProviderConfig::from_str("gemini").unwrap().type_name(),
            "gemini"
        );
        assert_eq!(ProviderConfig::from_str("groq").unwrap().type_name(), "groq");
        assert_eq!(
            ProviderConfig::from_str("openai_resp").unwrap().type_name(),
            "openai_resp"
        );
        assert_eq!(
            ProviderConfig::from_str("custom").unwrap().type_name(),
            "custom"
        );
        assert_eq!(
            ProviderConfig::from_str("openrouter").unwrap().type_name(),
            "open_router"
        );
        assert_eq!(
            ProviderConfig::from_str("github").unwrap().type_name(),
            "github_copilot"
        );
        assert_eq!(
            ProviderConfig::from_str("nvidia").unwrap().type_name(),
            "openai"
        );
        assert!(ProviderConfig::from_str("not-a-provider").is_err());
    }
}
