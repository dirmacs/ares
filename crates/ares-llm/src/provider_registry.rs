//! Provider Registry for managing multiple LLM providers
//!
//! This module provides a registry for managing named LLM providers
//! that can be configured via TOML configuration.
//!
//! # Model Capabilities (DIR-43)
//!
//! The registry now supports capability-based model selection:
//!
//! ```rust,ignore
//! use ares::llm::{ProviderRegistry, CapabilityRequirements};
//!
//! let requirements = CapabilityRequirements::builder()
//!     .requires_tools()
//!     .requires_vision()
//!     .min_context_window(100_000)
//!     .build();
//!
//! let model = registry.find_model(&requirements)?;
//! let client = registry.create_client_for_model(&model.name).await?;
//! ```

use crate::capabilities::{CapabilityRequirements, ModelCapabilities, ModelWithCapabilities};
use crate::client::{LLMClient, Provider};
use ares_types::types::{AppError, Result};
use ares_config::toml_config::{AresConfig, ModelConfig, ProviderConfig};
use std::collections::HashMap;
use std::sync::Arc;

/// Registry for managing multiple named LLM providers
///
/// The ProviderRegistry holds references to provider configurations and allows
/// creating LLM clients for specific models or providers by name.
pub struct ProviderRegistry {
    /// Provider configurations keyed by name
    providers: HashMap<String, ProviderConfig>,
    /// Model configurations keyed by name
    models: HashMap<String, ModelConfig>,
    /// Default model name to use when none specified
    default_model: Option<String>,
}

impl ProviderRegistry {
    /// Create a new empty provider registry
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            models: HashMap::new(),
            default_model: None,
        }
    }

    /// Create a provider registry from TOML configuration
    pub fn from_config(config: &AresConfig) -> Self {
        Self {
            providers: config.providers.clone(),
            models: config.models.clone(),
            default_model: config.models.keys().next().cloned(),
        }
    }

    /// Set the default model name
    pub fn set_default_model(&mut self, model_name: &str) {
        self.default_model = Some(model_name.to_string());
    }

    /// Register a provider configuration
    pub fn register_provider(&mut self, name: &str, config: ProviderConfig) {
        self.providers.insert(name.to_string(), config);
    }

    /// Register a model configuration
    pub fn register_model(&mut self, name: &str, config: ModelConfig) {
        self.models.insert(name.to_string(), config);
    }

    /// Remove a provider by name.
    pub fn unregister_provider(&mut self, name: &str) -> Option<ProviderConfig> {
        self.providers.remove(name)
    }

    /// Remove a model by name.
    pub fn unregister_model(&mut self, name: &str) -> Option<ModelConfig> {
        self.models.remove(name)
    }

    /// Get a provider configuration by name
    pub fn get_provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    /// Get a model configuration by name
    pub fn get_model(&self, name: &str) -> Option<&ModelConfig> {
        self.models.get(name)
    }

    /// Get all provider names
    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }

    /// Get all model names
    pub fn model_names(&self) -> Vec<&str> {
        self.models.keys().map(|s| s.as_str()).collect()
    }

    /// Create an LLM client for a specific model by name
    ///
    /// This resolves the model -> provider chain and creates the appropriate client.
    pub async fn create_client_for_model(&self, model_name: &str) -> Result<Box<dyn LLMClient>> {
        let model_config = self.get_model(model_name).ok_or_else(|| {
            AppError::Configuration(format!("Model '{}' not found in configuration", model_name))
        })?;

        let provider_config = self.get_provider(&model_config.provider).ok_or_else(|| {
            AppError::Configuration(format!(
                "Provider '{}' referenced by model '{}' not found",
                model_config.provider, model_name
            ))
        })?;

        let provider = Provider::from_model_config(model_config, provider_config)?;
        provider.create_client().await
    }

    /// Create an LLM client for a specific provider by name
    ///
    /// Uses the provider's default model.
    pub async fn create_client_for_provider(
        &self,
        provider_name: &str,
    ) -> Result<Box<dyn LLMClient>> {
        let provider_config = self.get_provider(provider_name).ok_or_else(|| {
            AppError::Configuration(format!(
                "Provider '{}' not found in configuration",
                provider_name
            ))
        })?;

        let provider = Provider::from_config(provider_config, None)?;
        provider.create_client().await
    }

    /// Create an LLM client using the default model
    pub async fn create_default_client(&self) -> Result<Box<dyn LLMClient>> {
        let model_name = self
            .default_model
            .as_ref()
            .ok_or_else(|| AppError::Configuration("No default model configured".into()))?;

        self.create_client_for_model(model_name).await
    }

    /// Check if a model exists in the registry
    pub fn has_model(&self, name: &str) -> bool {
        self.models.contains_key(name)
    }

    /// Check if a provider exists in the registry
    pub fn has_provider(&self, name: &str) -> bool {
        self.providers.contains_key(name)
    }

    // ================== Capability-Based Model Selection (DIR-43) ==================

    /// Get capabilities for a registered model.
    ///
    /// Attempts to auto-detect capabilities based on the model name,
    /// or returns default capabilities if unknown.
    pub fn get_model_capabilities(&self, model_name: &str) -> Option<ModelCapabilities> {
        let model_config = self.get_model(model_name)?;
        let provider_config = self.get_provider(&model_config.provider)?;

        // Start with auto-detected capabilities based on model ID
        let mut caps = ModelCapabilities::for_model(&model_config.model);

        // Override with provider-specific info
        match provider_config {
            ProviderConfig::Ollama { .. } => {
                caps.is_local = true;
                caps.cost_tier = "free".to_string();
            }
            ProviderConfig::LlamaCpp { .. } => {
                caps.is_local = true;
                caps.cost_tier = "free".to_string();
            }
            ProviderConfig::OpenAI { .. } => {
                caps.is_local = false;
            }
            ProviderConfig::Anthropic { .. } => {
                caps.is_local = false;
            }
        }

        Some(caps)
    }

    /// Get all models with their capabilities.
    pub fn models_with_capabilities(&self) -> Vec<ModelWithCapabilities> {
        self.models
            .iter()
            .filter_map(|(name, config)| {
                let caps = self.get_model_capabilities(name)?;
                Some(ModelWithCapabilities {
                    name: name.clone(),
                    provider: config.provider.clone(),
                    model_id: config.model.clone(),
                    capabilities: caps,
                })
            })
            .collect()
    }

    /// Find models that satisfy the given capability requirements.
    ///
    /// Returns matching models sorted by score (best match first).
    pub fn find_models(&self, requirements: &CapabilityRequirements) -> Vec<ModelWithCapabilities> {
        let mut matches: Vec<_> = self
            .models_with_capabilities()
            .into_iter()
            .filter(|m| m.capabilities.satisfies(requirements))
            .collect();

        // Sort by score (highest first)
        matches.sort_by(|a, b| {
            let score_a = a.capabilities.score(requirements);
            let score_b = b.capabilities.score(requirements);
            score_b.cmp(&score_a)
        });

        matches
    }

    /// Find the best model for the given requirements.
    ///
    /// Returns the highest-scoring model that satisfies all requirements,
    /// or None if no model matches.
    pub fn find_best_model(
        &self,
        requirements: &CapabilityRequirements,
    ) -> Option<ModelWithCapabilities> {
        self.find_models(requirements).into_iter().next()
    }

    /// Create an LLM client for the best model matching requirements.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let requirements = CapabilityRequirements::builder()
    ///     .requires_tools()
    ///     .requires_vision()
    ///     .build();
    ///
    /// let client = registry.create_client_for_requirements(&requirements).await?;
    /// ```
    pub async fn create_client_for_requirements(
        &self,
        requirements: &CapabilityRequirements,
    ) -> Result<Box<dyn LLMClient>> {
        let model = self.find_best_model(requirements).ok_or_else(|| {
            AppError::Configuration(format!(
                "No model found matching requirements: {:?}",
                requirements
            ))
        })?;

        self.create_client_for_model(&model.name).await
    }

    /// Find models suitable for agent tasks (tool calling required).
    pub fn find_agent_models(&self) -> Vec<ModelWithCapabilities> {
        self.find_models(&CapabilityRequirements::for_agent())
    }

    /// Find models suitable for vision tasks.
    pub fn find_vision_models(&self) -> Vec<ModelWithCapabilities> {
        self.find_models(&CapabilityRequirements::for_vision())
    }

    /// Find models suitable for coding tasks.
    pub fn find_coding_models(&self) -> Vec<ModelWithCapabilities> {
        self.find_models(&CapabilityRequirements::for_coding())
    }

    /// Find local-only models.
    pub fn find_local_models(&self) -> Vec<ModelWithCapabilities> {
        self.find_models(&CapabilityRequirements::for_local())
    }

    /// List all registered models with their provider info.
    pub fn list_models(&self) -> Vec<ModelInfo> {
        self.models
            .iter()
            .map(|(name, config)| ModelInfo {
                name: name.clone(),
                provider: config.provider.clone(),
                model: config.model.clone(),
            })
            .collect()
    }
}

/// Model info for listing available models via API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub provider: String,
    pub model: String,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration-based LLM client factory using the provider registry
///
/// This is the new factory that uses TOML configuration instead of environment variables.
pub struct ConfigBasedLLMFactory {
    registry: Arc<ProviderRegistry>,
    default_model: String,
}

impl ConfigBasedLLMFactory {
    /// Create a new factory from a provider registry
    pub fn new(registry: Arc<ProviderRegistry>, default_model: &str) -> Self {
        Self {
            registry,
            default_model: default_model.to_string(),
        }
    }

    /// Create a factory from TOML configuration
    pub fn from_config(config: &AresConfig) -> Result<Self> {
        let registry = ProviderRegistry::from_config(config);

        // Get the first model as default, or error if no models defined
        let default_model =
            config.models.keys().next().cloned().ok_or_else(|| {
                AppError::Configuration("No models defined in configuration".into())
            })?;

        Ok(Self {
            registry: Arc::new(registry),
            default_model,
        })
    }

    /// Get the provider registry
    pub fn registry(&self) -> &Arc<ProviderRegistry> {
        &self.registry
    }

    /// Create an LLM client for a specific model
    pub async fn create_for_model(&self, model_name: &str) -> Result<Box<dyn LLMClient>> {
        self.registry.create_client_for_model(model_name).await
    }

    /// Create an LLM client using the default model
    pub async fn create_default(&self) -> Result<Box<dyn LLMClient>> {
        self.registry
            .create_client_for_model(&self.default_model)
            .await
    }

    /// Get the default model name
    pub fn default_model(&self) -> &str {
        &self.default_model
    }

    /// Set the default model name
    pub fn set_default_model(&mut self, model_name: &str) {
        self.default_model = model_name.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::CapabilityRequirements;

    use ares_config::toml_config::{
        AresConfig, AuthConfig, BillingConfig, DatabaseConfig, DynamicConfigPaths, RagConfig,
        ServerConfig,
    };
    use std::collections::HashMap;

    fn sample_ollama_provider() -> ProviderConfig {
        ProviderConfig::Ollama {
            base_url: "http://localhost:11434".to_string(),
            default_model: "ministral-3:3b".to_string(),
        }
    }

    fn sample_model_config(provider: &str, model: &str) -> ModelConfig {
        ModelConfig {
            provider: provider.to_string(),
            model: model.to_string(),
            temperature: 0.7,
            max_tokens: 512,
            top_p: Some(0.9),
            frequency_penalty: Some(0.1),
            presence_penalty: Some(0.2),
        }
    }

    fn minimal_ares_config(
        providers: HashMap<String, ProviderConfig>,
        models: HashMap<String, ModelConfig>,
    ) -> AresConfig {
        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            database: DatabaseConfig::default(),
            providers,
            models,
            tools: HashMap::new(),
            agents: HashMap::new(),
            workflows: HashMap::new(),
            rag: RagConfig::default(),
            billing: BillingConfig::default(),
            config: DynamicConfigPaths::default(),
        }
    }

    fn assert_configuration_error<T>(result: Result<T>, expected_substring: &str) {
        match result {
            Err(AppError::Configuration(msg)) => {
                assert!(
                    msg.contains(expected_substring),
                    "expected message containing {expected_substring:?}, got {msg:?}"
                );
            }
            Err(other) => panic!("expected Configuration error, got: {other:?}"),
            Ok(_) => panic!(
                "expected Configuration error containing {expected_substring:?}, got Ok"
            ),
        }
    }


    #[test]
    fn test_empty_registry() {
        let registry = ProviderRegistry::new();
        assert!(registry.provider_names().is_empty());
        assert!(registry.model_names().is_empty());
    }

    #[test]
    fn test_register_provider() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "ollama-local",
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".to_string(),
                default_model: "ministral-3:3b".to_string(),
            },
        );

        assert!(registry.has_provider("ollama-local"));
        assert!(!registry.has_provider("nonexistent"));
    }

    #[test]
    fn test_register_model() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "ollama-local",
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".to_string(),
                default_model: "ministral-3:3b".to_string(),
            },
        );
        registry.register_model(
            "fast",
            ModelConfig {
                provider: "ollama-local".to_string(),
                model: "ministral-3:3b".to_string(),
                temperature: 0.7,
                max_tokens: 256,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
            },
        );

        assert!(registry.has_model("fast"));
        assert!(!registry.has_model("nonexistent"));
    }

    // ================== DIR-43: Capability Tests ==================

    fn create_test_registry() -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();

        // Register providers
        registry.register_provider(
            "ollama",
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".to_string(),
                default_model: "llama-3.3-70b-instruct".to_string(),
            },
        );

        registry.register_provider(
            "anthropic",
            ProviderConfig::Anthropic {
                api_key_env: "ANTHROPIC_API_KEY".to_string(),
                default_model: "claude-3-5-sonnet-20241022".to_string(),
            },
        );

        registry.register_provider(
            "openai",
            ProviderConfig::OpenAI {
                api_key_env: "OPENAI_API_KEY".to_string(),
                api_base: "https://api.openai.com/v1".to_string(),
                default_model: "gpt-4o".to_string(),
            },
        );

        // Register models
        registry.register_model(
            "fast-local",
            ModelConfig {
                provider: "ollama".to_string(),
                model: "ministral-3:3b".to_string(),
                temperature: 0.7,
                max_tokens: 512,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
            },
        );

        registry.register_model(
            "powerful-local",
            ModelConfig {
                provider: "ollama".to_string(),
                model: "llama-3.3-70b-instruct".to_string(),
                temperature: 0.7,
                max_tokens: 2048,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
            },
        );

        registry.register_model(
            "claude-sonnet",
            ModelConfig {
                provider: "anthropic".to_string(),
                model: "claude-3-5-sonnet-20241022".to_string(),
                temperature: 0.7,
                max_tokens: 4096,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
            },
        );

        registry.register_model(
            "gpt4o",
            ModelConfig {
                provider: "openai".to_string(),
                model: "gpt-4o-2024-08-06".to_string(),
                temperature: 0.7,
                max_tokens: 4096,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
            },
        );

        registry
    }

    #[test]
    fn test_get_model_capabilities() {
        let registry = create_test_registry();

        // Test local model capabilities
        let fast_caps = registry.get_model_capabilities("fast-local").unwrap();
        assert!(fast_caps.is_local);
        assert_eq!(fast_caps.cost_tier, "free");
        assert!(fast_caps.supports_tools);

        // Test cloud model capabilities
        let claude_caps = registry.get_model_capabilities("claude-sonnet").unwrap();
        assert!(!claude_caps.is_local);
        assert!(claude_caps.supports_tools);
        assert!(claude_caps.supports_vision);
        assert_eq!(claude_caps.context_window, 200_000);
    }

    #[test]
    fn test_models_with_capabilities() {
        let registry = create_test_registry();
        let models = registry.models_with_capabilities();

        assert_eq!(models.len(), 4);

        // Verify all models have capabilities
        for model in &models {
            assert!(!model.name.is_empty());
            assert!(!model.provider.is_empty());
            // All these models should support tools
            assert!(model.capabilities.supports_tools);
        }
    }

    #[test]
    fn test_find_local_models() {
        let registry = create_test_registry();
        let local_models = registry.find_local_models();

        // Should find the two Ollama models
        assert_eq!(local_models.len(), 2);
        for model in &local_models {
            assert!(model.capabilities.is_local);
            assert_eq!(model.capabilities.cost_tier, "free");
        }
    }

    #[test]
    fn test_find_vision_models() {
        let registry = create_test_registry();
        let vision_models = registry.find_vision_models();

        // Claude and GPT-4o support vision
        assert_eq!(vision_models.len(), 2);
        for model in &vision_models {
            assert!(model.capabilities.supports_vision);
        }
    }

    #[test]
    fn test_find_best_model_for_agent() {
        let registry = create_test_registry();

        let requirements = CapabilityRequirements::for_agent();
        let best = registry.find_best_model(&requirements);

        assert!(best.is_some());
        let best = best.unwrap();
        assert!(best.capabilities.supports_tools);
        assert!(best.capabilities.production_ready);
    }

    #[test]
    fn test_find_best_model_with_context_window() {
        let registry = create_test_registry();

        // Require large context window
        let requirements = CapabilityRequirements::builder()
            .min_context_window(100_000)
            .build();

        let matches = registry.find_models(&requirements);

        // Should match Claude (200k), GPT-4o (128k), and Llama (128k)
        assert!(matches.len() >= 2);
        for model in &matches {
            assert!(model.capabilities.context_window >= 100_000);
        }
    }

    #[test]
    fn test_find_best_model_prefers_cheaper() {
        let registry = create_test_registry();

        // Basic requirements that all models satisfy
        let requirements = CapabilityRequirements::builder().requires_tools().build();

        let best = registry.find_best_model(&requirements).unwrap();

        // Should prefer local/free models when all else is equal
        // (scoring penalizes cost)
        assert!(
            best.capabilities.is_local || best.capabilities.cost_tier == "free",
            "Expected best model to be local/free, got: {} (cost: {})",
            best.name,
            best.capabilities.cost_tier
        );
    }

    #[test]
    fn test_no_model_matches_impossible_requirements() {
        let registry = create_test_registry();

        // Impossible requirements: local + vision (no local vision models in test registry)
        let requirements = CapabilityRequirements::builder()
            .requires_local()
            .requires_vision()
            .build();

        let matches = registry.find_models(&requirements);
        assert!(matches.is_empty());
    }

    #[test]
    fn test_find_coding_models() {
        let registry = create_test_registry();
        let coding_models = registry.find_coding_models();

        // Should find models that support tools + reasoning + large context
        for model in &coding_models {
            assert!(model.capabilities.supports_tools);
            assert!(model.capabilities.supports_reasoning);
            assert!(model.capabilities.context_window >= 32_000);
        }
    }
    #[test]
    fn test_unregister_provider() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "ollama",
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".to_string(),
                default_model: "llama3".to_string(),
            },
        );
        assert!(registry.has_provider("ollama"));
        let removed = registry.unregister_provider("ollama").unwrap();
        assert!(matches!(removed, ProviderConfig::Ollama { .. }));
        assert!(!registry.has_provider("ollama"));
    }

    #[test]
    fn test_unregister_model() {
        let mut registry = create_test_registry();
        assert!(registry.has_model("fast-local"));
        registry.unregister_model("fast-local");
        assert!(!registry.has_model("fast-local"));
    }

    #[test]
    fn test_lookup_provider_by_name() {
        let registry = create_test_registry();
        let provider = registry.get_provider("openai").unwrap();
        assert!(matches!(provider, ProviderConfig::OpenAI { .. }));
        assert!(registry.get_provider("missing").is_none());
    }

    #[test]
    fn test_default_registry() {
        let registry = ProviderRegistry::default();
        assert!(registry.provider_names().is_empty());
        assert!(registry.model_names().is_empty());
    }

    #[test]
    fn test_register_provider_overwrites_existing() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "ollama",
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".to_string(),
                default_model: "old-model".to_string(),
            },
        );
        registry.register_provider(
            "ollama",
            ProviderConfig::Ollama {
                base_url: "http://127.0.0.1:11434".to_string(),
                default_model: "new-model".to_string(),
            },
        );

        let provider = registry.get_provider("ollama").unwrap();
        if let ProviderConfig::Ollama { default_model, .. } = provider {
            assert_eq!(default_model, "new-model");
        } else {
            panic!("expected Ollama provider");
        }
    }

    #[test]
    fn test_provider_and_model_name_iteration() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider("alpha", sample_ollama_provider());
        registry.register_provider("beta", sample_ollama_provider());
        registry.register_model("m1", sample_model_config("alpha", "model-a"));
        registry.register_model("m2", sample_model_config("beta", "model-b"));

        let mut provider_names = registry.provider_names();
        provider_names.sort_unstable();
        assert_eq!(provider_names, vec!["alpha", "beta"]);

        let mut model_names = registry.model_names();
        model_names.sort_unstable();
        assert_eq!(model_names, vec!["m1", "m2"]);
    }

    #[test]
    fn test_lookup_model_by_name() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider("ollama", sample_ollama_provider());
        registry.register_model("fast", sample_model_config("ollama", "ministral-3:3b"));

        let model = registry.get_model("fast").unwrap();
        assert_eq!(model.provider, "ollama");
        assert_eq!(model.model, "ministral-3:3b");
        assert!(registry.get_model("missing").is_none());
    }

    #[test]
    fn test_list_models_returns_registered_entries() {
        let registry = create_test_registry();
        let models = registry.list_models();

        assert_eq!(models.len(), 4);
        assert!(models.iter().any(|m| m.name == "fast-local" && m.provider == "ollama"));
        assert!(models.iter().any(|m| m.name == "gpt4o" && m.provider == "openai"));
    }

    #[test]
    fn test_from_config_loads_providers_and_models() {
        let mut providers = HashMap::new();
        providers.insert("ollama".to_string(), sample_ollama_provider());
        let mut models = HashMap::new();
        models.insert("fast".to_string(), sample_model_config("ollama", "ministral-3:3b"));

        let config = minimal_ares_config(providers, models);
        let registry = ProviderRegistry::from_config(&config);

        assert!(registry.has_provider("ollama"));
        assert!(registry.has_model("fast"));
        assert_eq!(registry.model_names(), vec!["fast"]);
    }

    #[tokio::test]
    async fn test_set_default_model() {
        let mut registry = create_test_registry();
        registry.set_default_model("claude-sonnet");
        match registry.create_default_client().await {
            Err(AppError::Configuration(msg)) => {
                assert!(!msg.contains("No default model configured"), "got: {msg}");
            }
            Err(other) => panic!("expected Configuration error, got: {other:?}"),
            Ok(_) => panic!("expected Configuration error, but client creation succeeded"),
        }
    }

    #[test]
    fn test_get_model_capabilities_unknown_model() {
        let registry = create_test_registry();
        assert!(registry.get_model_capabilities("missing").is_none());
    }

    #[test]
    fn test_get_model_capabilities_missing_provider() {
        let mut registry = ProviderRegistry::new();
        registry.register_model(
            "orphan",
            sample_model_config("missing-provider", "some-model"),
        );
        assert!(registry.get_model_capabilities("orphan").is_none());
    }

    #[test]
    fn test_unregister_provider_missing_returns_none() {
        let mut registry = ProviderRegistry::new();
        assert!(registry.unregister_provider("missing").is_none());
    }

    #[test]
    fn test_unregister_model_returns_removed_config() {
        let mut registry = create_test_registry();
        let removed = registry.unregister_model("fast-local").unwrap();
        assert_eq!(removed.provider, "ollama");
        assert_eq!(removed.model, "ministral-3:3b");
        assert!(registry.unregister_model("fast-local").is_none());
    }

    #[test]
    fn test_provider_config_serde_roundtrip() {
        let configs = [
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".to_string(),
                default_model: "llama3".to_string(),
            },
            ProviderConfig::OpenAI {
                api_key_env: "OPENAI_API_KEY".to_string(),
                api_base: "https://api.openai.com/v1".to_string(),
                default_model: "gpt-4o".to_string(),
            },
            ProviderConfig::Anthropic {
                api_key_env: "ANTHROPIC_API_KEY".to_string(),
                default_model: "claude-3-5-sonnet-20241022".to_string(),
            },
            ProviderConfig::LlamaCpp {
                model_path: "./models/test.gguf".to_string(),
                n_ctx: 8192,
                n_threads: 8,
                max_tokens: 1024,
            },
        ];

        for original in configs {
            let json = serde_json::to_string(&original).unwrap();
            let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(original.type_name(), decoded.type_name());
        }
    }

    #[test]
    fn test_model_config_serde_roundtrip() {
        let original = sample_model_config("ollama", "ministral-3:3b");
        let json = serde_json::to_string(&original).unwrap();
        let decoded: ModelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.provider, original.provider);
        assert_eq!(decoded.model, original.model);
        assert_eq!(decoded.temperature, original.temperature);
        assert_eq!(decoded.max_tokens, original.max_tokens);
        assert_eq!(decoded.top_p, original.top_p);
        assert_eq!(decoded.frequency_penalty, original.frequency_penalty);
        assert_eq!(decoded.presence_penalty, original.presence_penalty);
    }

    #[test]
    fn test_config_factory_from_config() {
        let mut providers = HashMap::new();
        providers.insert("ollama".to_string(), sample_ollama_provider());
        let mut models = HashMap::new();
        models.insert("fast".to_string(), sample_model_config("ollama", "ministral-3:3b"));

        let config = minimal_ares_config(providers, models);
        let factory = ConfigBasedLLMFactory::from_config(&config).unwrap();
        assert_eq!(factory.default_model(), "fast");
        assert!(factory.registry().has_model("fast"));
    }

    #[test]
    fn test_config_factory_from_config_no_models() {
        let config = minimal_ares_config(HashMap::new(), HashMap::new());
        assert_configuration_error(
            ConfigBasedLLMFactory::from_config(&config),
            "No models defined in configuration",
        );
    }

    #[tokio::test]
    async fn test_create_client_for_model_not_found() {
        let registry = ProviderRegistry::new();
        assert_configuration_error(
            registry.create_client_for_model("missing").await,
            "Model 'missing' not found in configuration",
        );
    }

    #[tokio::test]
    async fn test_create_client_for_model_missing_provider() {
        let mut registry = ProviderRegistry::new();
        registry.register_model(
            "orphan",
            sample_model_config("missing-provider", "some-model"),
        );
        assert_configuration_error(
            registry.create_client_for_model("orphan").await,
            "Provider 'missing-provider' referenced by model 'orphan' not found",
        );
    }

    #[tokio::test]
    async fn test_create_client_for_provider_not_found() {
        let registry = ProviderRegistry::new();
        assert_configuration_error(
            registry.create_client_for_provider("missing").await,
            "Provider 'missing' not found in configuration",
        );
    }

    #[tokio::test]
    async fn test_create_default_client_without_default_model() {
        let registry = ProviderRegistry::new();
        assert_configuration_error(
            registry.create_default_client().await,
            "No default model configured",
        );
    }

    #[tokio::test]
    async fn test_create_client_for_requirements_no_match() {
        let registry = create_test_registry();
        let requirements = CapabilityRequirements::builder()
            .requires_local()
            .requires_vision()
            .build();
        assert_configuration_error(
            registry.create_client_for_requirements(&requirements).await,
            "No model found matching requirements",
        );
    }


}
