//! Provider Registry for managing multiple LLM providers
//!
//! This module provides a registry for managing named LLM providers
//! that can be configured via TOML configuration.

use crate::llm::client::{LLMClient, Provider};
use crate::types::{AppError, Result};
use crate::utils::toml_config::{AresConfig, ModelConfig, ProviderConfig};
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
        let model_name = self.default_model.as_ref().ok_or_else(|| {
            AppError::Configuration("No default model configured".into())
        })?;

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
        let default_model = config
            .models
            .keys()
            .next()
            .cloned()
            .ok_or_else(|| AppError::Configuration("No models defined in configuration".into()))?;

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
                default_model: "llama3.2".to_string(),
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
                default_model: "llama3.2".to_string(),
            },
        );
        registry.register_model(
            "fast",
            ModelConfig {
                provider: "ollama-local".to_string(),
                model: "llama3.2:1b".to_string(),
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
}
