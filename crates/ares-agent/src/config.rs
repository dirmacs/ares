use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============= Agent Configuration =============

/// Agent configuration binding a model to tools and behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Reference to a model name defined in \[models\].
    pub model: String,

    /// System prompt for the agent (personality, instructions).
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// List of tool names this agent can use.
    #[serde(default)]
    pub tools: Vec<String>,

    /// Optional whitelist of tool names this agent is allowed to use.
    /// If absent, all tools are permitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,

    /// Maximum tool calling iterations before stopping (default: 10).
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: usize,

    /// Whether to execute tool calls in parallel when possible.
    #[serde(default)]
    pub parallel_tools: bool,

    /// Enable per-session history compaction ([`ares_llm::Compactor`]).
    ///
    /// Off by default; when on, long conversations are maintained as a
    /// bounded working set (critical facts + rolling memory + recent turns)
    /// instead of a naive last-5 history slice.
    #[serde(default)]
    pub compaction_enabled: Option<bool>,

    /// Sampling temperature override (0.0..=2.0). None = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Maximum output tokens override (>0). None = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Stop sequences. None = provider default; Some(vec) sent verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,

    /// Nucleus sampling override (0.0..=1.0). None = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// Frequency penalty override (-2.0..=2.0). None = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,

    /// Presence penalty override (-2.0..=2.0). None = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,

    /// Additional agent-specific configuration passed through.
    #[serde(flatten)]
    pub extra: HashMap<String, toml::Value>,
}

fn default_max_tool_iterations() -> usize {
    10
}
