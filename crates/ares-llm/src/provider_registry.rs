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
use crate::client::{LLMClient, ModelParams, Provider};
use ares_types::types::{AppError, Result};
use ares_config::toml_config::{AresConfig, ModelConfig, ProviderConfig};
use ares_config::nvidia_catalog::{CatalogEntry, NvidiaCatalogCache, NvidiaConfig};
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::sync::Arc;

/// Runtime provider entry, synthesized from the DB `runtime_providers` table.
#[derive(Debug, Clone)]
pub struct RuntimeProviderEntry {
    /// Display name for UI purposes.
    pub display_name: String,
    /// Provider compatibility type: "openai-compatible", "anthropic-compatible", "custom".
    pub provider_type: String,
    /// Base URL for the API.
    pub api_base: String,
    /// Authentication type: "api_key", "oauth2", "aws_sigv4".
    pub auth_type: String,
    /// Default model when none is specified.
    pub default_model: Option<String>,
    /// Extra HTTP headers.
    pub headers: HashMap<String, String>,
    /// Resolved API key (populated by the reload path).
    pub api_key: Option<String>,
    /// Whether this runtime provider is enabled.
    pub enabled: bool,
}

/// Registry for managing multiple named LLM providers
///
/// The ProviderRegistry holds references to provider configurations and allows
/// creating LLM clients for specific models or providers by name.
pub struct ProviderRegistry {
    /// Provider configurations keyed by name (legacy, kept for backward compat).
    providers: HashMap<String, ProviderConfig>,
    /// Explicit model configurations keyed by name (legacy, kept for backward compat).
    models: HashMap<String, ModelConfig>,
    /// Live NVIDIA catalog cache.
    catalog: Option<Arc<NvidiaCatalogCache>>,
    /// Default model name to use when none specified.
    default_model: Option<String>,
    /// Runtime providers loaded from the DB (hot-swapped).
    runtime_providers: Arc<ArcSwap<HashMap<String, RuntimeProviderEntry>>>,
}

impl ProviderRegistry {
    /// Create a new empty provider registry
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            models: HashMap::new(),
            catalog: None,
            default_model: None,
            runtime_providers: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        }
    }

    /// Create a provider registry from TOML configuration
    pub fn from_config(config: &AresConfig) -> Self {
        let mut providers = config.providers.clone();

        // If no legacy providers are configured, synthesize a single NVIDIA provider.
        if providers.is_empty() {
            let nvidia = config.nvidia.clone().unwrap_or_default();
            let _ = std::env::var(&nvidia.api_key_env); // we don't error here; refresh will report it
            providers.insert(
                "nvidia".to_string(),
                ProviderConfig::OpenAI {
                    api_key_env: nvidia.api_key_env.clone(),
                    api_base: nvidia.api_base.clone(),
                    default_model: nvidia.default_model.clone(),
                },
            );
        }

        let default_model = config
            .nvidia
            .as_ref()
            .map(|n| n.default_model.clone())
            .or_else(|| config.models.keys().next().cloned());

        Self {
            providers,
            models: config.models.clone(),
            catalog: None,
            default_model,
            runtime_providers: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        }
    }

    /// Hot-swap the runtime provider map. Called by admin endpoints after
    /// mutating the DB so the new providers are visible immediately.
    pub fn reload_runtime_providers(&self, providers: Vec<RuntimeProviderEntry>, names: Vec<String>) {
        let mut map = HashMap::new();
        for (entry, name) in providers.into_iter().zip(names.into_iter()) {
            if entry.enabled {
                map.insert(name, entry);
            }
        }
        self.runtime_providers.store(Arc::new(map));
    }

    /// Attach a live catalog cache (used after construction for background refresh).
    pub fn with_catalog(mut self, catalog: Arc<NvidiaCatalogCache>) -> Self {
        self.catalog = Some(catalog);
        self
    }

    /// Set the default model name
    pub fn set_default_model(&mut self, model_name: &str) {
        self.default_model = Some(model_name.to_string());
    }

    /// Register a provider configuration (legacy no-op if providers are already managed).
    pub fn register_provider(&mut self, name: &str, config: ProviderConfig) {
        self.providers.insert(name.to_string(), config);
    }

    /// Register a model configuration (legacy backward-compat).
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

    /// Get a provider configuration by name.
    ///
    /// Runtime providers are checked first and synthesized into a [`ProviderConfig`]
    /// on the fly; legacy static configs are checked second. The return value is
    /// cloned so that the caller owns it — this is required because runtime
    /// providers are materialised from the arc-swapped map rather than stored as
    /// [`ProviderConfig`] internally.
    pub fn get_provider(&self, name: &str) -> Option<ProviderConfig> {
        let runtime = self.runtime_providers.load();
        if let Some(entry) = runtime.get(name) {
            return Some(Self::synthesize_provider_config(entry));
        }
        self.providers.get(name).cloned()
    }

    /// Synthesize a legacy [`ProviderConfig`] from a runtime provider entry.
    fn synthesize_provider_config(entry: &RuntimeProviderEntry) -> ProviderConfig {
        match entry.provider_type.as_str() {
            "anthropic-compatible" => ProviderConfig::Anthropic {
                api_key_env: entry.api_key.clone().unwrap_or_else(|| "ANTHROPIC_API_KEY".to_string()),
                default_model: entry.default_model.clone().unwrap_or_default(),
            },
            _ => ProviderConfig::OpenAI {
                api_key_env: entry.api_key.clone().unwrap_or_else(|| "OPENAI_API_KEY".to_string()),
                api_base: entry.api_base.clone(),
                default_model: entry.default_model.clone().unwrap_or_default(),
            },
        }
    }

    /// Get a model configuration by name.
    /// Checks explicit legacy models first, then falls back to the live catalog.
    pub fn get_model(&self, name: &str) -> Option<ModelConfig> {
        // 1. explicit legacy models
        if let Some(cfg) = self.models.get(name) {
            return Some(cfg.clone());
        }
        // 2. catalog lookup – synthesize a ModelConfig on the fly
        if let Some(ref catalog) = self.catalog {
            let snapshot = catalog.snapshot();
            if snapshot.iter().any(|e| e.id == name) {
                return Some(ModelConfig {
                    provider: "nvidia".to_string(),
                    model: name.to_string(),
                    temperature: 0.7,
                    max_tokens: 512,
                });
            }
        }
        None
    }

    /// Get all provider names (legacy + runtime).
    pub fn provider_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.keys().cloned().collect();
        let runtime = self.runtime_providers.load();
        for name in runtime.keys() {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    /// Get all model names (legacy + catalog ids)
    pub fn model_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.models.keys().cloned().collect();
        if let Some(ref catalog) = self.catalog {
            for entry in catalog.snapshot() {
                names.push(entry.id.clone());
            }
        }
        names
    }

    /// Create an LLM client for a specific model by name
    pub async fn create_client_for_model(&self, model_name: &str) -> Result<Box<dyn LLMClient>> {
        // 1. Try legacy explicit models first
        if let Some(model_config) = self.models.get(model_name) {
            let provider_config = self.get_provider(&model_config.provider).ok_or_else(|| {
                AppError::Configuration(format!(
                    "Provider '{}' referenced by model '{}' not found",
                    model_config.provider, model_name
                ))
            })?;
            let provider = Provider::from_model_config(model_config, &provider_config)?;
            return provider.create_client().await;
        }

        // 2. Try catalog lookup
        if let Some(ref catalog) = self.catalog {
            let snapshot = catalog.snapshot();
            if snapshot.iter().any(|e| e.id == model_name) {
                let nvidia_cfg = self.nvidia_config_from_providers();
                let provider_config = ProviderConfig::OpenAI {
                    api_key_env: nvidia_cfg.api_key_env,
                    api_base: nvidia_cfg.api_base,
                    default_model: model_name.to_string(),
                };
                let provider = Provider::from_config(&provider_config, Some(model_name))?;
                return provider.create_client().await;
            }
        }

        Err(AppError::Configuration(format!(
            "Model '{}' not found in configuration",
            model_name
        )))
    }

    /// Create an LLM client for a specific provider by name
    pub async fn create_client_for_provider(
        &self,
        provider_name: &str,
    ) -> Result<Box<dyn LLMClient>> {
        // Check runtime providers first so headers and custom base URLs are preserved.
        if let Some(entry) = self.runtime_providers.load().get(provider_name) {
            let provider_config = Self::synthesize_provider_config(entry);
            let provider = Provider::from_config(&provider_config, None)?;
            return provider.create_client().await;
        }

        let provider_config = self.providers.get(provider_name).ok_or_else(|| {
            AppError::Configuration(format!(
                "Provider '{}' not found in configuration",
                provider_name
            ))
        })?;

        let provider = Provider::from_config(&provider_config, None)?;
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
            || self
                .catalog
                .as_ref()
                .map(|c| c.snapshot().iter().any(|e| e.id == name))
                .unwrap_or(false)
    }

    /// Check if a provider exists in the registry (legacy or runtime).
    pub fn has_provider(&self, name: &str) -> bool {
        self.providers.contains_key(name) || self.runtime_providers.load().contains_key(name)
    }

    // ================== Capability-Based Model Selection (DIR-43) ==================

    /// Get capabilities for a registered model.
    pub fn get_model_capabilities(&self, model_name: &str) -> Option<ModelCapabilities> {
        // If it's a legacy model, use the explicit config
        if let Some(model_config) = self.models.get(model_name) {
            let provider_config = self.get_provider(&model_config.provider)?;
            let mut caps = ModelCapabilities::for_model(&model_config.model);
            if matches!(provider_config, ProviderConfig::OpenAI { .. }) {
                caps.is_local = false;
            }
            return Some(caps);
        }

        // If it's in the catalog, use the catalog id directly
        if let Some(ref catalog) = self.catalog {
            let snapshot = catalog.snapshot();
            if snapshot.iter().any(|e| e.id == model_name) {
                let mut caps = ModelCapabilities::for_model(model_name);
                caps.is_local = false;
                return Some(caps);
            }
        }

        None
    }

    /// Get all models with their capabilities.
    pub fn models_with_capabilities(&self) -> Vec<ModelWithCapabilities> {
        let mut result = Vec::new();

        // Legacy models
        for (name, config) in &self.models {
            if let Some(caps) = self.get_model_capabilities(name) {
                result.push(ModelWithCapabilities {
                    name: name.clone(),
                    provider: config.provider.clone(),
                    model_id: config.model.clone(),
                    capabilities: caps,
                });
            }
        }

        // Catalog models
        if let Some(ref catalog) = self.catalog {
            for entry in catalog.snapshot() {
                let caps = self.get_model_capabilities(&entry.id).unwrap_or_else(|| {
                    let mut c = ModelCapabilities::for_model(&entry.id);
                    c.is_local = false;
                    c
                });
                result.push(ModelWithCapabilities {
                    name: entry.id.clone(),
                    provider: "nvidia".to_string(),
                    model_id: entry.id.clone(),
                    capabilities: caps,
                });
            }
        }

        result
    }

    /// Find models that satisfy the given capability requirements.
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
    pub fn find_best_model(
        &self,
        requirements: &CapabilityRequirements,
    ) -> Option<ModelWithCapabilities> {
        self.find_models(requirements).into_iter().next()
    }

    /// Create an LLM client for the best model matching requirements.
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
        let mut models = Vec::new();

        // Legacy explicit models
        for (name, config) in &self.models {
            models.push(ModelInfo {
                name: name.clone(),
                provider: config.provider.clone(),
                model: config.model.clone(),
                owned_by: config.provider.clone(),
                quality_score: 75,
                is_chat: true,
            });
        }

        // Catalog entries
        if let Some(ref catalog) = self.catalog {
            let snapshot = catalog.snapshot();
            if !snapshot.is_empty() {
                for entry in snapshot {
                    models.push(ModelInfo {
                        name: entry.id.clone(),
                        provider: "nvidia".to_string(),
                        model: entry.id.clone(),
                        owned_by: entry.owned_by.clone(),
                        quality_score: entry.quality_score,
                        is_chat: true,
                    });
                }
            } else if let Some(ref default) = self.default_model {
                // Fallback when catalog is empty: expose the default model so the UI is never blank
                models.push(ModelInfo {
                    name: default.clone(),
                    provider: "nvidia".to_string(),
                    model: default.clone(),
                    owned_by: "unknown".to_string(),
                    quality_score: 75,
                    is_chat: true,
                });
            }
        } else if let Some(ref default) = self.default_model {
            // No catalog at all – still expose the default model
            models.push(ModelInfo {
                name: default.clone(),
                provider: "nvidia".to_string(),
                model: default.clone(),
                owned_by: "unknown".to_string(),
                quality_score: 75,
                is_chat: true,
            });
        }

        models
    }

    // ============================================================
    // Helpers
    // ============================================================

    /// Resolve a model tier or model name to a chain of `(provider_name,
    /// ProviderConfig)` pairs, following the fallback chain stored in
    /// `fleet_secrets` for the primary provider.
    ///
    /// 1. Looks up `tenant_model_tiers` for the tenant + tier.
    /// 2. Falls back to the registry's configured models.
    /// 3. Falls back to treating `tier_or_model` as a provider name.
    /// 4. Loads the primary provider's `fallback_providers` from fleet secrets
    ///    and appends each resolved configuration.
    #[cfg(feature = "postgres")]
    pub async fn resolve_with_fallback(
        &self,
        tier_or_model: &str,
        tenant_id: &str,
        pool: &sqlx::PgPool,
        fleet_secrets: &ares_config::fleet_secrets::FleetSecrets,
    ) -> Vec<(String, ProviderConfig)> {
        use ares_db::tenant_model_tiers::TenantModelTierStore;
        use std::collections::HashSet;

        // 1. Resolve primary provider name.
        let primary_provider = {
            let store = TenantModelTierStore::new(pool);
            if let Ok(Some(tier)) = store.get(tenant_id, tier_or_model).await {
                tier.provider_name
            } else if let Some(model_cfg) = self.get_model(tier_or_model) {
                model_cfg.provider
            } else if self.has_provider(tier_or_model) {
                tier_or_model.to_string()
            } else {
                return Vec::new();
            }
        };

        let mut result = Vec::new();
        let mut seen = HashSet::new();

        // 2. Primary provider config.
        if let Some(cfg) = self.get_provider(&primary_provider) {
            seen.insert(primary_provider.clone());
            result.push((primary_provider.clone(), cfg));
        }

        // 3. Fallback chain from fleet secrets.
        if let Some(override_) = fleet_secrets.get(&primary_provider) {
            for fallback_name in &override_.fallback_providers {
                if seen.contains(fallback_name) {
                    continue;
                }
                if let Some(cfg) = self.get_provider(fallback_name) {
                    seen.insert(fallback_name.clone());
                    result.push((fallback_name.clone(), cfg));
                }
            }
        }

        result
    }

    /// Extract NVIDIA config from the synthetic provider we inserted.
    fn nvidia_config_from_providers(&self) -> NvidiaConfig {
        if let Some(ProviderConfig::OpenAI {
            api_key_env,
            api_base,
            default_model,
        }) = self.providers.get("nvidia")
        {
            NvidiaConfig {
                api_key_env: api_key_env.clone(),
                api_base: api_base.clone(),
                models_url: format!("{}/models", api_base.trim_end_matches('/')),
                catalog_refresh_seconds: 3600,
                default_model: default_model.clone(),
            }
        } else {
            NvidiaConfig::default()
        }
    }
}

/// Model info for listing available models via API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub owned_by: String,
    #[serde(default)]
    pub quality_score: u8,
    #[serde(default)]
    pub is_chat: bool,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration-based LLM client factory using the provider registry
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

        let default_model = config
            .nvidia
            .as_ref()
            .map(|n| n.default_model.clone())
            .or_else(|| config.models.keys().next().cloned())
            .unwrap_or_else(|| "nvidia/nemotron-3-ultra-550b-a55b".to_string());

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

    fn sample_openai_provider() -> ProviderConfig {
        ProviderConfig::OpenAI {
            api_key_env: "TEST_KEY".to_string(),
            api_base: "https://test.example.com/v1".to_string(),
            default_model: "test-model".to_string(),
        }
    }

    fn sample_model_config(provider: &str, model: &str) -> ModelConfig {
        ModelConfig {
            provider: provider.to_string(),
            model: model.to_string(),
            temperature: 0.7,
            max_tokens: 512,
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
            nvidia: None,
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
            "nvidia",
            ProviderConfig::OpenAI {
                api_key_env: "TEST_KEY".to_string(),
                api_base: "https://test.example.com/v1".to_string(),
                default_model: "test-model".to_string(),
            },
        );

        assert!(registry.has_provider("nvidia"));
        assert!(!registry.has_provider("nonexistent"));
    }

    #[test]
    fn test_register_model() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "nvidia",
            ProviderConfig::OpenAI {
                api_key_env: "TEST_KEY".to_string(),
                api_base: "https://test.example.com/v1".to_string(),
                default_model: "test-model".to_string(),
            },
        );
        registry.register_model(
            "fast",
            ModelConfig {
                provider: "nvidia".to_string(),
                model: "test-model".to_string(),
                temperature: 0.7,
                max_tokens: 256,
            },
        );

        assert!(registry.has_model("fast"));
        assert!(!registry.has_model("nonexistent"));
    }

    // ================== DIR-43: Capability Tests ==================

    fn create_test_registry() -> ProviderRegistry {
        let mut registry = ProviderRegistry::new();

        registry.register_provider(
            "nvidia",
            ProviderConfig::OpenAI {
                api_key_env: "TEST_KEY".to_string(),
                api_base: "https://integrate.api.nvidia.com/v1".to_string(),
                default_model: "nvidia/nemotron-3-ultra-550b-a55b".to_string(),
            },
        );

        registry.register_model(
            "fast-local",
            ModelConfig {
                provider: "nvidia".to_string(),
                model: "nvidia/nemotron-3-ultra-550b-a55b".to_string(),
                temperature: 0.7,
                max_tokens: 512,
            },
        );

        registry.register_model(
            "powerful-local",
            ModelConfig {
                provider: "nvidia".to_string(),
                model: "nvidia/nemotron-3-ultra-550b-a55b".to_string(),
                temperature: 0.7,
                max_tokens: 2048,
            },
        );

        registry.register_model(
            "qwen",
            ModelConfig {
                provider: "nvidia".to_string(),
                model: "qwen/qwen-32b".to_string(),
                temperature: 0.7,
                max_tokens: 4096,
            },
        );

        registry
    }

    #[test]
    fn test_get_model_capabilities() {
        let registry = create_test_registry();

        let fast_caps = registry.get_model_capabilities("fast-local").unwrap();
        assert!(!fast_caps.is_local);
        assert!(fast_caps.supports_tools);

    }

    #[test]
    fn test_models_with_capabilities() {
        let registry = create_test_registry();
        let models = registry.models_with_capabilities();

        assert_eq!(models.len(), 3);

        for model in &models {
            assert!(!model.name.is_empty());
            assert!(!model.provider.is_empty());
            assert!(model.capabilities.supports_tools);
        }
    }

    #[test]
    fn test_find_local_models() {
        let registry = create_test_registry();
        let local_models = registry.find_local_models();
        // NVIDIA models are not local
        assert!(local_models.is_empty());
    }

    #[test]
    fn test_find_vision_models() {
        let registry = create_test_registry();
        let vision_models = registry.find_vision_models();
        // No explicit vision models in test registry
        assert!(vision_models.is_empty());
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

        let requirements = CapabilityRequirements::builder()
            .min_context_window(100_000)
            .build();

        let matches = registry.find_models(&requirements);

        assert!(matches.len() >= 2);
        for model in &matches {
            assert!(model.capabilities.context_window >= 100_000);
        }
    }

    #[test]
    fn test_find_best_model_prefers_cheaper() {
        let registry = create_test_registry();

        let requirements = CapabilityRequirements::builder().requires_tools().build();

        let best = registry.find_best_model(&requirements).unwrap();

        // NVIDIA models are "free" tier in our heuristic
        assert_eq!(best.capabilities.cost_tier, "free");
    }

    #[test]
    fn test_no_model_matches_impossible_requirements() {
        let registry = create_test_registry();

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
            "nvidia",
            ProviderConfig::OpenAI {
                api_key_env: "TEST_KEY".to_string(),
                api_base: "https://test.example.com/v1".to_string(),
                default_model: "test-model".to_string(),
            },
        );
        assert!(registry.has_provider("nvidia"));
        let removed = registry.unregister_provider("nvidia").unwrap();
        assert!(matches!(removed, ProviderConfig::OpenAI { .. }));
        assert!(!registry.has_provider("nvidia"));
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
        let provider = registry.get_provider("nvidia").unwrap();
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
            "nvidia",
            ProviderConfig::OpenAI {
                api_key_env: "TEST_KEY".to_string(),
                api_base: "https://old.example.com/v1".to_string(),
                default_model: "old-model".to_string(),
            },
        );
        registry.register_provider(
            "nvidia",
            ProviderConfig::OpenAI {
                api_key_env: "TEST_KEY".to_string(),
                api_base: "https://new.example.com/v1".to_string(),
                default_model: "new-model".to_string(),
            },
        );

        let provider = registry.get_provider("nvidia").unwrap();
        if let ProviderConfig::OpenAI { default_model, .. } = provider {
            assert_eq!(default_model, "new-model");
        } else {
            panic!("expected OpenAI provider");
        }
    }

    #[test]
    fn test_provider_and_model_name_iteration() {
        let mut registry = ProviderRegistry::new();
        registry.register_provider("alpha", sample_openai_provider());
        registry.register_provider("beta", sample_openai_provider());
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
        registry.register_provider("nvidia", sample_openai_provider());
        registry.register_model("fast", sample_model_config("nvidia", "test-model"));

        let model = registry.get_model("fast").unwrap();
        assert_eq!(model.provider, "nvidia");
        assert_eq!(model.model, "test-model");
        assert!(registry.get_model("missing").is_none());
    }

    #[test]
    fn test_list_models_returns_registered_entries() {
        let registry = create_test_registry();
        let models = registry.list_models();

        assert_eq!(models.len(), 3);
        assert!(models.iter().any(|m| m.name == "fast-local" && m.provider == "nvidia"));
    }

    #[test]
    fn test_from_config_loads_providers_and_models() {
        let mut providers = HashMap::new();
        providers.insert("nvidia".to_string(), sample_openai_provider());
        let mut models = HashMap::new();
        models.insert("fast".to_string(), sample_model_config("nvidia", "test-model"));

        let config = minimal_ares_config(providers, models);
        let registry = ProviderRegistry::from_config(&config);

        assert!(registry.has_provider("nvidia"));
        assert!(registry.has_model("fast"));
        assert_eq!(registry.model_names(), vec!["fast"]);
    }

    #[tokio::test]
    async fn test_set_default_model() {
        let mut registry = create_test_registry();
        registry.set_default_model("powerful-local");
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
        assert_eq!(removed.provider, "nvidia");
        assert_eq!(removed.model, "nvidia/nemotron-3-ultra-550b-a55b");
        assert!(registry.unregister_model("fast-local").is_none());
    }

    #[test]
    fn test_provider_config_serde_roundtrip() {
        let configs = [ProviderConfig::OpenAI {
            api_key_env: "OPENAI_API_KEY".to_string(),
            api_base: "https://api.openai.com/v1".to_string(),
            default_model: "gpt-4o".to_string(),
        }];

        for original in configs {
            let json = serde_json::to_string(&original).unwrap();
            let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(original.type_name(), decoded.type_name());
        }
    }

    #[test]
    fn test_model_config_serde_roundtrip() {
        let original = sample_model_config("nvidia", "test-model");
        let json = serde_json::to_string(&original).unwrap();
        let decoded: ModelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.provider, original.provider);
        assert_eq!(decoded.model, original.model);
        assert_eq!(decoded.temperature, original.temperature);
        assert_eq!(decoded.max_tokens, original.max_tokens);
    }

    #[test]
    fn test_config_factory_from_config() {
        let mut providers = HashMap::new();
        providers.insert("nvidia".to_string(), sample_openai_provider());
        let mut models = HashMap::new();
        models.insert("fast".to_string(), sample_model_config("nvidia", "test-model"));

        let config = minimal_ares_config(providers, models);
        let factory = ConfigBasedLLMFactory::from_config(&config).unwrap();
        assert_eq!(factory.default_model(), "fast");
        assert!(factory.registry().has_model("fast"));
    }

    #[test]
    fn test_config_factory_from_config_no_models() {
        let config = minimal_ares_config(HashMap::new(), HashMap::new());
        // Should now succeed by falling back to the hardcoded default
        let factory = ConfigBasedLLMFactory::from_config(&config).unwrap();
        assert_eq!(factory.default_model(), "nvidia/nemotron-3-ultra-550b-a55b");
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
