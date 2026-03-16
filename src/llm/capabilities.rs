//! Model Capabilities for DIR-43
//!
//! This module provides capability detection and matching for LLM models,
//! enabling intelligent model selection based on task requirements.
//!
//! # Example
//!
//! ```rust,ignore
//! use ares_server::llm::{ModelCapabilities, CapabilityRequirements, ProviderRegistry};
//!
//! // Define what capabilities the task needs
//! let requirements = CapabilityRequirements::builder()
//!     .requires_tools()
//!     .min_context_window(32_000)
//!     .build();
//!
//! // Find a matching model
//! let model = registry.find_model(&requirements)?;
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Capabilities that an LLM model may support.
///
/// These are used for intelligent model selection based on task requirements.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelCapabilities {
    /// Whether the model supports tool/function calling
    #[serde(default)]
    pub supports_tools: bool,

    /// Whether the model supports vision/image inputs
    #[serde(default)]
    pub supports_vision: bool,

    /// Whether the model supports audio inputs
    #[serde(default)]
    pub supports_audio: bool,

    /// Whether the model supports structured output (JSON mode)
    #[serde(default)]
    pub supports_json_mode: bool,

    /// Whether the model supports streaming responses
    #[serde(default = "default_true")]
    pub supports_streaming: bool,

    /// Whether the model supports system prompts
    #[serde(default = "default_true")]
    pub supports_system_prompt: bool,

    /// Maximum context window size in tokens
    #[serde(default = "default_context_window")]
    pub context_window: u32,

    /// Maximum output tokens the model can generate
    #[serde(default = "default_max_output")]
    pub max_output_tokens: u32,

    /// Whether the model has reasoning/chain-of-thought capabilities
    #[serde(default)]
    pub supports_reasoning: bool,

    /// Whether the model supports code execution
    #[serde(default)]
    pub supports_code_execution: bool,

    /// Cost tier: "free", "low", "medium", "high", "premium"
    #[serde(default = "default_cost_tier")]
    pub cost_tier: String,

    /// Speed tier: "slow", "medium", "fast", "realtime"
    #[serde(default = "default_speed_tier")]
    pub speed_tier: String,

    /// Quality tier: "basic", "standard", "high", "premium"
    #[serde(default = "default_quality_tier")]
    pub quality_tier: String,

    /// Supported languages (empty = all languages)
    #[serde(default)]
    pub languages: HashSet<String>,

    /// Model family (e.g., "gpt-4", "claude-3", "llama-3")
    #[serde(default)]
    pub family: Option<String>,

    /// Whether this model is suitable for production use
    #[serde(default = "default_true")]
    pub production_ready: bool,

    /// Whether this model runs locally (no API calls)
    #[serde(default)]
    pub is_local: bool,

    /// Custom capability tags for extension
    #[serde(default)]
    pub tags: HashSet<String>,
}

fn default_true() -> bool {
    true
}

fn default_context_window() -> u32 {
    4096
}

fn default_max_output() -> u32 {
    4096
}

fn default_cost_tier() -> String {
    "medium".to_string()
}

fn default_speed_tier() -> String {
    "medium".to_string()
}

fn default_quality_tier() -> String {
    "standard".to_string()
}

impl ModelCapabilities {
    /// Create capabilities for a known model by name.
    ///
    /// This provides sensible defaults for popular models.
    pub fn for_model(model_name: &str) -> Self {
        let model_lower = model_name.to_lowercase();

        // Claude models
        if model_lower.contains("claude-3-5-sonnet") || model_lower.contains("claude-sonnet-4") {
            return Self {
                supports_tools: true,
                supports_vision: true,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: 200_000,
                max_output_tokens: 8192,
                supports_reasoning: true,
                cost_tier: "high".to_string(),
                speed_tier: "fast".to_string(),
                quality_tier: "premium".to_string(),
                family: Some("claude-3".to_string()),
                production_ready: true,
                ..Default::default()
            };
        }

        if model_lower.contains("claude-3-opus") || model_lower.contains("claude-opus") {
            return Self {
                supports_tools: true,
                supports_vision: true,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: 200_000,
                max_output_tokens: 4096,
                supports_reasoning: true,
                cost_tier: "premium".to_string(),
                speed_tier: "slow".to_string(),
                quality_tier: "premium".to_string(),
                family: Some("claude-3".to_string()),
                production_ready: true,
                ..Default::default()
            };
        }

        if model_lower.contains("claude-3-haiku") || model_lower.contains("claude-haiku") {
            return Self {
                supports_tools: true,
                supports_vision: true,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: 200_000,
                max_output_tokens: 4096,
                supports_reasoning: false,
                cost_tier: "low".to_string(),
                speed_tier: "realtime".to_string(),
                quality_tier: "standard".to_string(),
                family: Some("claude-3".to_string()),
                production_ready: true,
                ..Default::default()
            };
        }

        // GPT models
        if model_lower.contains("gpt-4o") {
            return Self {
                supports_tools: true,
                supports_vision: true,
                supports_audio: true,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: 128_000,
                max_output_tokens: 16384,
                supports_reasoning: true,
                cost_tier: "high".to_string(),
                speed_tier: "fast".to_string(),
                quality_tier: "premium".to_string(),
                family: Some("gpt-4".to_string()),
                production_ready: true,
                ..Default::default()
            };
        }

        if model_lower.contains("gpt-4-turbo") || model_lower.contains("gpt-4-1106") {
            return Self {
                supports_tools: true,
                supports_vision: true,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: 128_000,
                max_output_tokens: 4096,
                supports_reasoning: true,
                cost_tier: "high".to_string(),
                speed_tier: "medium".to_string(),
                quality_tier: "premium".to_string(),
                family: Some("gpt-4".to_string()),
                production_ready: true,
                ..Default::default()
            };
        }

        if model_lower.contains("gpt-4") && !model_lower.contains("gpt-4o") {
            return Self {
                supports_tools: true,
                supports_vision: false,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: 8192,
                max_output_tokens: 4096,
                supports_reasoning: true,
                cost_tier: "high".to_string(),
                speed_tier: "slow".to_string(),
                quality_tier: "premium".to_string(),
                family: Some("gpt-4".to_string()),
                production_ready: true,
                ..Default::default()
            };
        }

        if model_lower.contains("gpt-3.5") {
            return Self {
                supports_tools: true,
                supports_vision: false,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: 16385,
                max_output_tokens: 4096,
                supports_reasoning: false,
                cost_tier: "low".to_string(),
                speed_tier: "fast".to_string(),
                quality_tier: "standard".to_string(),
                family: Some("gpt-3.5".to_string()),
                production_ready: true,
                ..Default::default()
            };
        }

        // Llama models
        if model_lower.contains("llama-3.3") || model_lower.contains("llama-3.1") {
            let context = if model_lower.contains("70b") {
                128_000
            } else {
                131_072
            };
            return Self {
                supports_tools: true,
                supports_vision: false,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: context,
                max_output_tokens: 4096,
                supports_reasoning: model_lower.contains("70b"),
                cost_tier: "free".to_string(),
                speed_tier: "medium".to_string(),
                quality_tier: if model_lower.contains("70b") {
                    "high".to_string()
                } else {
                    "standard".to_string()
                },
                family: Some("llama-3".to_string()),
                production_ready: true,
                is_local: true,
                ..Default::default()
            };
        }

        // Mistral models
        if model_lower.contains("ministral") || model_lower.contains("mistral") {
            return Self {
                supports_tools: true,
                supports_vision: false,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: 32_000,
                max_output_tokens: 4096,
                supports_reasoning: false,
                cost_tier: "low".to_string(),
                speed_tier: "fast".to_string(),
                quality_tier: "standard".to_string(),
                family: Some("mistral".to_string()),
                production_ready: true,
                is_local: true,
                ..Default::default()
            };
        }

        // Qwen models
        if model_lower.contains("qwen") {
            let has_vl = model_lower.contains("-vl");
            return Self {
                supports_tools: true,
                supports_vision: has_vl,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: 128_000,
                max_output_tokens: 8192,
                supports_reasoning: model_lower.contains("qwq") || model_lower.contains("235b"),
                cost_tier: "free".to_string(),
                speed_tier: "medium".to_string(),
                quality_tier: "high".to_string(),
                family: Some("qwen".to_string()),
                production_ready: true,
                is_local: true,
                ..Default::default()
            };
        }

        // DeepSeek models
        if model_lower.contains("deepseek") {
            return Self {
                supports_tools: true,
                supports_vision: false,
                supports_json_mode: true,
                supports_streaming: true,
                supports_system_prompt: true,
                context_window: 128_000,
                max_output_tokens: 8192,
                supports_reasoning: model_lower.contains("v3") || model_lower.contains("r1"),
                cost_tier: "low".to_string(),
                speed_tier: "medium".to_string(),
                quality_tier: "high".to_string(),
                family: Some("deepseek".to_string()),
                production_ready: true,
                ..Default::default()
            };
        }

        // Default capabilities for unknown models
        Self::default()
    }

    /// Check if this model satisfies the given requirements.
    pub fn satisfies(&self, requirements: &CapabilityRequirements) -> bool {
        // Check boolean requirements
        if requirements.requires_tools && !self.supports_tools {
            return false;
        }
        if requirements.requires_vision && !self.supports_vision {
            return false;
        }
        if requirements.requires_audio && !self.supports_audio {
            return false;
        }
        if requirements.requires_json_mode && !self.supports_json_mode {
            return false;
        }
        if requirements.requires_streaming && !self.supports_streaming {
            return false;
        }
        if requirements.requires_reasoning && !self.supports_reasoning {
            return false;
        }
        if requirements.requires_code_execution && !self.supports_code_execution {
            return false;
        }
        if requirements.requires_local && !self.is_local {
            return false;
        }
        if requirements.requires_production_ready && !self.production_ready {
            return false;
        }

        // Check numeric requirements
        if let Some(min_context) = requirements.min_context_window {
            if self.context_window < min_context {
                return false;
            }
        }
        if let Some(min_output) = requirements.min_output_tokens {
            if self.max_output_tokens < min_output {
                return false;
            }
        }

        // Check tier requirements
        if let Some(ref max_cost) = requirements.max_cost_tier {
            if !tier_satisfies(&self.cost_tier, max_cost) {
                return false;
            }
        }
        if let Some(ref min_speed) = requirements.min_speed_tier {
            if !tier_satisfies(min_speed, &self.speed_tier) {
                return false;
            }
        }
        if let Some(ref min_quality) = requirements.min_quality_tier {
            if !tier_satisfies(min_quality, &self.quality_tier) {
                return false;
            }
        }

        // Check required tags
        for tag in &requirements.required_tags {
            if !self.tags.contains(tag) {
                return false;
            }
        }

        // Check excluded families
        if let Some(ref family) = self.family {
            if requirements.excluded_families.contains(family) {
                return false;
            }
        }

        true
    }

    /// Calculate a score for how well this model matches requirements.
    ///
    /// Higher score = better match. Used for ranking when multiple models satisfy requirements.
    pub fn score(&self, requirements: &CapabilityRequirements) -> u32 {
        let mut score = 0u32;

        // Bonus for exceeding minimum requirements
        if let Some(min_context) = requirements.min_context_window {
            score += (self.context_window.saturating_sub(min_context)) / 1000;
        }

        // Bonus for speed when not explicitly required
        score += match self.speed_tier.as_str() {
            "realtime" => 40,
            "fast" => 30,
            "medium" => 20,
            "slow" => 10,
            _ => 0,
        };

        // Bonus for quality
        score += match self.quality_tier.as_str() {
            "premium" => 40,
            "high" => 30,
            "standard" => 20,
            "basic" => 10,
            _ => 0,
        };

        // Penalty for cost (prefer cheaper when quality is equal)
        score += match self.cost_tier.as_str() {
            "free" => 50,
            "low" => 40,
            "medium" => 30,
            "high" => 20,
            "premium" => 10,
            _ => 0,
        };

        // Bonus for local models (no network latency/cost)
        if self.is_local {
            score += 20;
        }

        // Bonus for having more capabilities than required
        if self.supports_tools && !requirements.requires_tools {
            score += 5;
        }
        if self.supports_reasoning && !requirements.requires_reasoning {
            score += 10;
        }

        score
    }
}

/// Requirements for model capability matching.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityRequirements {
    /// Require tool/function calling support
    #[serde(default)]
    pub requires_tools: bool,

    /// Require vision/image input support
    #[serde(default)]
    pub requires_vision: bool,

    /// Require audio input support
    #[serde(default)]
    pub requires_audio: bool,

    /// Require JSON mode/structured output support
    #[serde(default)]
    pub requires_json_mode: bool,

    /// Require streaming response support
    #[serde(default)]
    pub requires_streaming: bool,

    /// Require reasoning/chain-of-thought capabilities
    #[serde(default)]
    pub requires_reasoning: bool,

    /// Require code execution support
    #[serde(default)]
    pub requires_code_execution: bool,

    /// Require the model to run locally
    #[serde(default)]
    pub requires_local: bool,

    /// Require production-ready models only
    #[serde(default)]
    pub requires_production_ready: bool,

    /// Minimum context window size
    pub min_context_window: Option<u32>,

    /// Minimum output token capacity
    pub min_output_tokens: Option<u32>,

    /// Maximum cost tier (e.g., "medium" means free/low/medium are OK)
    pub max_cost_tier: Option<String>,

    /// Minimum speed tier
    pub min_speed_tier: Option<String>,

    /// Minimum quality tier
    pub min_quality_tier: Option<String>,

    /// Required custom tags
    #[serde(default)]
    pub required_tags: HashSet<String>,

    /// Model families to exclude (e.g., exclude GPT for privacy reasons)
    #[serde(default)]
    pub excluded_families: HashSet<String>,
}

impl CapabilityRequirements {
    /// Create a new builder for capability requirements.
    pub fn builder() -> CapabilityRequirementsBuilder {
        CapabilityRequirementsBuilder::default()
    }

    /// Create requirements for tool-calling agents.
    pub fn for_agent() -> Self {
        Self {
            requires_tools: true,
            requires_production_ready: true,
            min_quality_tier: Some("standard".to_string()),
            ..Default::default()
        }
    }

    /// Create requirements for chat/conversation.
    pub fn for_chat() -> Self {
        Self {
            requires_streaming: true,
            requires_production_ready: true,
            ..Default::default()
        }
    }

    /// Create requirements for code generation.
    pub fn for_coding() -> Self {
        Self {
            requires_tools: true,
            requires_reasoning: true,
            min_context_window: Some(32_000),
            min_quality_tier: Some("high".to_string()),
            ..Default::default()
        }
    }

    /// Create requirements for vision tasks.
    pub fn for_vision() -> Self {
        Self {
            requires_vision: true,
            requires_production_ready: true,
            ..Default::default()
        }
    }

    /// Create requirements for local-only inference.
    pub fn for_local() -> Self {
        Self {
            requires_local: true,
            max_cost_tier: Some("free".to_string()),
            ..Default::default()
        }
    }
}

/// Builder for CapabilityRequirements.
#[derive(Debug, Default)]
pub struct CapabilityRequirementsBuilder {
    inner: CapabilityRequirements,
}

impl CapabilityRequirementsBuilder {
    /// Require tool/function calling support.
    pub fn requires_tools(mut self) -> Self {
        self.inner.requires_tools = true;
        self
    }

    /// Require vision/image input support.
    pub fn requires_vision(mut self) -> Self {
        self.inner.requires_vision = true;
        self
    }

    /// Require audio input support.
    pub fn requires_audio(mut self) -> Self {
        self.inner.requires_audio = true;
        self
    }

    /// Require JSON mode support.
    pub fn requires_json_mode(mut self) -> Self {
        self.inner.requires_json_mode = true;
        self
    }

    /// Require streaming support.
    pub fn requires_streaming(mut self) -> Self {
        self.inner.requires_streaming = true;
        self
    }

    /// Require reasoning capabilities.
    pub fn requires_reasoning(mut self) -> Self {
        self.inner.requires_reasoning = true;
        self
    }

    /// Require code execution support.
    pub fn requires_code_execution(mut self) -> Self {
        self.inner.requires_code_execution = true;
        self
    }

    /// Require local-only models.
    pub fn requires_local(mut self) -> Self {
        self.inner.requires_local = true;
        self
    }

    /// Require production-ready models.
    pub fn requires_production_ready(mut self) -> Self {
        self.inner.requires_production_ready = true;
        self
    }

    /// Set minimum context window size.
    pub fn min_context_window(mut self, tokens: u32) -> Self {
        self.inner.min_context_window = Some(tokens);
        self
    }

    /// Set minimum output token capacity.
    pub fn min_output_tokens(mut self, tokens: u32) -> Self {
        self.inner.min_output_tokens = Some(tokens);
        self
    }

    /// Set maximum cost tier.
    pub fn max_cost_tier(mut self, tier: impl Into<String>) -> Self {
        self.inner.max_cost_tier = Some(tier.into());
        self
    }

    /// Set minimum speed tier.
    pub fn min_speed_tier(mut self, tier: impl Into<String>) -> Self {
        self.inner.min_speed_tier = Some(tier.into());
        self
    }

    /// Set minimum quality tier.
    pub fn min_quality_tier(mut self, tier: impl Into<String>) -> Self {
        self.inner.min_quality_tier = Some(tier.into());
        self
    }

    /// Add a required tag.
    pub fn require_tag(mut self, tag: impl Into<String>) -> Self {
        self.inner.required_tags.insert(tag.into());
        self
    }

    /// Exclude a model family.
    pub fn exclude_family(mut self, family: impl Into<String>) -> Self {
        self.inner.excluded_families.insert(family.into());
        self
    }

    /// Build the requirements.
    pub fn build(self) -> CapabilityRequirements {
        self.inner
    }
}

/// Check if tier `a` satisfies requirement for tier `b`.
///
/// Tier ordering: free < low < medium < high < premium
fn tier_satisfies(requirement: &str, actual: &str) -> bool {
    let tier_order = |t: &str| match t.to_lowercase().as_str() {
        "free" | "realtime" | "basic" => 0,
        "low" | "fast" | "standard" => 1,
        "medium" => 2,
        "high" | "slow" => 3,
        "premium" => 4,
        _ => 2, // Default to medium
    };

    tier_order(actual) >= tier_order(requirement)
}

/// Model with its capabilities for registry storage.
#[derive(Debug, Clone)]
pub struct ModelWithCapabilities {
    /// Model configuration name
    pub name: String,
    /// Provider name
    pub provider: String,
    /// Model identifier for the provider
    pub model_id: String,
    /// Model capabilities
    pub capabilities: ModelCapabilities,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_capabilities() {
        let caps = ModelCapabilities::for_model("claude-3-5-sonnet-20241022");
        assert!(caps.supports_tools);
        assert!(caps.supports_vision);
        assert_eq!(caps.context_window, 200_000);
        assert_eq!(caps.quality_tier, "premium");
    }

    #[test]
    fn test_gpt4o_capabilities() {
        let caps = ModelCapabilities::for_model("gpt-4o-2024-08-06");
        assert!(caps.supports_tools);
        assert!(caps.supports_vision);
        assert!(caps.supports_audio);
        assert_eq!(caps.context_window, 128_000);
    }

    #[test]
    fn test_llama_capabilities() {
        let caps = ModelCapabilities::for_model("llama-3.3-70b-instruct");
        assert!(caps.supports_tools);
        assert!(!caps.supports_vision);
        assert!(caps.is_local);
        assert_eq!(caps.cost_tier, "free");
    }

    #[test]
    fn test_requirements_builder() {
        let reqs = CapabilityRequirements::builder()
            .requires_tools()
            .requires_vision()
            .min_context_window(100_000)
            .max_cost_tier("high")
            .build();

        assert!(reqs.requires_tools);
        assert!(reqs.requires_vision);
        assert_eq!(reqs.min_context_window, Some(100_000));
        assert_eq!(reqs.max_cost_tier, Some("high".to_string()));
    }

    #[test]
    fn test_capability_matching() {
        let claude = ModelCapabilities::for_model("claude-3-5-sonnet-20241022");
        let gpt35 = ModelCapabilities::for_model("gpt-3.5-turbo");

        let vision_reqs = CapabilityRequirements::builder().requires_vision().build();

        assert!(claude.satisfies(&vision_reqs));
        assert!(!gpt35.satisfies(&vision_reqs));
    }

    #[test]
    fn test_context_window_matching() {
        let claude = ModelCapabilities::for_model("claude-3-5-sonnet-20241022");
        let gpt4 = ModelCapabilities::for_model("gpt-4");

        let long_context_reqs = CapabilityRequirements::builder()
            .min_context_window(100_000)
            .build();

        assert!(claude.satisfies(&long_context_reqs));
        assert!(!gpt4.satisfies(&long_context_reqs)); // gpt-4 base has 8k context
    }

    #[test]
    fn test_local_model_matching() {
        let llama = ModelCapabilities::for_model("llama-3.3-70b-instruct");
        let claude = ModelCapabilities::for_model("claude-3-5-sonnet-20241022");

        let local_reqs = CapabilityRequirements::for_local();

        assert!(llama.satisfies(&local_reqs));
        assert!(!claude.satisfies(&local_reqs));
    }

    #[test]
    fn test_scoring() {
        let claude = ModelCapabilities::for_model("claude-3-5-sonnet-20241022");
        let haiku = ModelCapabilities::for_model("claude-3-haiku-20240307");
        let llama = ModelCapabilities::for_model("llama-3.3-70b-instruct");

        let basic_reqs = CapabilityRequirements::builder().requires_tools().build();

        // All satisfy the basic requirements
        assert!(claude.satisfies(&basic_reqs));
        assert!(haiku.satisfies(&basic_reqs));
        assert!(llama.satisfies(&basic_reqs));

        // Llama should score higher due to being free + local
        let claude_score = claude.score(&basic_reqs);
        let llama_score = llama.score(&basic_reqs);
        assert!(
            llama_score > claude_score,
            "Llama (free, local) should score higher than Claude (high cost)"
        );

        // Haiku should score higher than Claude due to lower cost and faster speed
        let haiku_score = haiku.score(&basic_reqs);
        assert!(
            haiku_score > claude_score,
            "Haiku should score higher than Sonnet due to cost/speed"
        );
    }

    #[test]
    fn test_preset_requirements() {
        let agent_reqs = CapabilityRequirements::for_agent();
        assert!(agent_reqs.requires_tools);
        assert!(agent_reqs.requires_production_ready);

        let coding_reqs = CapabilityRequirements::for_coding();
        assert!(coding_reqs.requires_tools);
        assert!(coding_reqs.requires_reasoning);
        assert_eq!(coding_reqs.min_context_window, Some(32_000));

        let vision_reqs = CapabilityRequirements::for_vision();
        assert!(vision_reqs.requires_vision);
    }

    #[test]
    fn test_tier_comparison() {
        assert!(tier_satisfies("low", "medium")); // medium >= low
        assert!(tier_satisfies("medium", "high")); // high >= medium
        assert!(!tier_satisfies("high", "low")); // low < high
        assert!(tier_satisfies("standard", "premium")); // premium >= standard
    }
}
