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
#[cfg(feature = "genai")]
use crate::client::GenaiProvider;
#[cfg(feature = "genai")]
use genai::adapter::AdapterKind;
use crate::config::{ModelConfig, ProviderConfig};
use crate::nvidia_catalog::{NvidiaCatalogCache, NvidiaConfig};
use arc_swap::ArcSwap;
use ares_types::types::{AppError, Result};
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;
// Phase 3 unified hot-reload: re-export ReflectService from core (single source)
pub use cordis::ReflectService;

/// Runtime provider entry, synthesized from the DB `runtime_providers` table.
#[derive(Debug, Clone)]
pub struct RuntimeProviderEntry {
    /// Optional tenant owner. `None` means fleet-wide.
    pub tenant_id: Option<String>,
    /// Display name for UI purposes.
    pub display_name: String,
    /// Provider compatibility type: "openai-compatible", "anthropic-compatible", "bedrock", "custom".
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

/// Resolved provider plus the concrete model id that must be sent to that provider.
#[derive(Debug, Clone)]
pub struct ResolvedProviderConfig {
    pub provider_name: String,
    pub model_name: String,
    pub provider_config: ProviderConfig,
    pub params: ModelParams,
    pub tenant_id: Option<String>,
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
    runtime_providers: Arc<ArcSwap<HashMap<String, Vec<RuntimeProviderEntry>>>>,
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
    pub fn from_config(
        providers: std::collections::HashMap<String, ProviderConfig>,
        models: std::collections::HashMap<String, ModelConfig>,
        nvidia: Option<&NvidiaConfig>,
    ) -> Self {
        let mut providers = providers;

        // If no legacy providers are configured, synthesize a single NVIDIA provider.
        if providers.is_empty() {
            let nvidia = nvidia.cloned().unwrap_or_default();
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

        providers
            .entry("bedrock".to_string())
            .or_insert_with(Self::default_bedrock_provider_config);

        providers
            .entry("azure".to_string())
            .or_insert_with(Self::default_azure_provider_config);

        let default_model = nvidia
            .map(|n| n.default_model.clone())
            .or_else(|| models.keys().next().cloned());

        Self {
            providers,
            models,
            catalog: None,
            default_model,
            runtime_providers: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        }
    }

    /// Hot-swap the runtime provider map. Called by admin endpoints after
    /// mutating the DB so the new providers are visible immediately.
    pub fn reload_runtime_providers(
        &self,
        providers: Vec<RuntimeProviderEntry>,
        names: Vec<String>,
    ) {
        let mut map = HashMap::new();
        for (entry, name) in providers.into_iter().zip(names) {
            if entry.enabled {
                map.entry(name).or_insert_with(Vec::new).push(entry);
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
        self.provider_for_tenant(name, None)
    }

    /// Crate-private tenant lookup. The former public tenant-provider getter
    /// is gone; callers use `Llm::get_client` (or `get_provider_for_ctx` inside
    /// this crate). Tenant-scoped runtime providers are only visible to their
    /// owning tenant; fleet-wide runtime providers and static providers remain
    /// visible to every tenant.
    pub(crate) fn provider_for_tenant(
        &self,
        name: &str,
        tenant_id: Option<&str>,
    ) -> Option<ProviderConfig> {
        if let Some(entry) = self.runtime_provider_entry_for_tenant(name, tenant_id) {
            return Some(Self::synthesize_provider_config(&entry));
        }
        self.providers.get(name).cloned()
    }

    /// Resolve a provider visible to the tenant derived from the context's isolate
    /// namespace. Reads `ctx.isolate_label(TypeId::of::<Llm>())` and
    /// strips a leading `tenant:`/`user:` prefix (mirroring
    /// `ares_agent::resolver::user_id_from_ctx`), delegating to
    /// crate-private tenant lookup with the derived tenant (`None` when unlabeled).
    pub fn get_provider_for_ctx(
        &self,
        ctx: &std::sync::Arc<cordis::Context>,
        name: &str,
    ) -> Option<ProviderConfig> {
        let tenant = tenant_from_ctx(ctx);
        self.provider_for_tenant(name, tenant.as_deref())
    }

    fn runtime_provider_entry_for_tenant(
        &self,
        name: &str,
        tenant_id: Option<&str>,
    ) -> Option<RuntimeProviderEntry> {
        let runtime = self.runtime_providers.load();
        let entries = runtime.get(name)?;
        if let Some(requester) = tenant_id {
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry.tenant_id.as_deref() == Some(requester))
            {
                return Some(entry.clone());
            }
        }
        entries
            .iter()
            .find(|entry| entry.tenant_id.is_none())
            .cloned()
    }

    /// Synthesize a legacy [`ProviderConfig`] from a runtime provider entry.
    fn synthesize_provider_config(entry: &RuntimeProviderEntry) -> ProviderConfig {
        match entry.provider_type.as_str() {
            "anthropic-compatible" => ProviderConfig::Anthropic {
                api_key_env: entry
                    .api_key
                    .clone()
                    .unwrap_or_else(|| "ANTHROPIC_API_KEY".to_string()),
                default_model: entry.default_model.clone().unwrap_or_default(),
            },
            "bedrock" | "bedrock-compatible" => ProviderConfig::Bedrock {
                api_key_env: "AWS_BEARER_TOKEN_BEDROCK".to_string(),
                region_env: entry
                    .headers
                    .get("region_env")
                    .cloned()
                    .unwrap_or_else(|| "AWS_REGION".to_string()),
                default_model: entry.default_model.clone().unwrap_or_default(),
            },
            "azure" | "azure-compatible" => ProviderConfig::Azure {
                api_key_env: "AZURE_FOUNDRY_API_KEY".to_string(),
                base_url_env: "AZURE_FOUNDRY_BASE_URL".to_string(),
                default_model: entry.default_model.clone().unwrap_or_default(),
            },
            _ => ProviderConfig::OpenAI {
                api_key_env: entry
                    .api_key
                    .clone()
                    .unwrap_or_else(|| "OPENAI_API_KEY".to_string()),
                api_base: entry.api_base.clone(),
                default_model: entry.default_model.clone().unwrap_or_default(),
            },
        }
    }

    #[allow(dead_code)]
    fn runtime_api_key(provider_name: &str, entry: &RuntimeProviderEntry) -> Result<String> {
        entry
            .api_key
            .as_ref()
            .filter(|api_key| !api_key.is_empty())
            .cloned()
            .ok_or_else(|| {
                AppError::Configuration(format!(
                    "Runtime provider '{}' API key is not resolved",
                    provider_name
                ))
            })
    }

    fn provider_from_runtime_entry(
        provider_name: &str,
        entry: &RuntimeProviderEntry,
    ) -> Result<Provider> {
        Self::provider_from_runtime_entry_with_params(
            provider_name,
            entry,
            entry.default_model.as_deref(),
            ModelParams::default(),
        )
    }

    #[allow(dead_code)]
    fn provider_default_model(config: &ProviderConfig) -> &str {
        config.default_model()
    }

    fn default_bedrock_provider_config() -> ProviderConfig {
        ProviderConfig::Bedrock {
            api_key_env: "AWS_BEARER_TOKEN_BEDROCK".to_string(),
            region_env: "AWS_REGION".to_string(),
            default_model: "us.anthropic.claude-haiku-4-5-20251001-v1:0".to_string(),
        }
    }

    fn default_azure_provider_config() -> ProviderConfig {
        ProviderConfig::Azure {
            api_key_env: "AZURE_FOUNDRY_API_KEY".to_string(),
            base_url_env: "AZURE_FOUNDRY_BASE_URL".to_string(),
            default_model: "DeepSeek-V4-Flash".to_string(),
        }
    }

    fn bedrock_model_id_from_name(model_name: &str) -> Option<&str> {
        let trimmed = model_name.trim();
        if let Some(model_id) = trimmed.strip_prefix("bedrock/") {
            return (!model_id.trim().is_empty()).then_some(model_id.trim());
        }
        if trimmed.starts_with("us.anthropic.") || trimmed.starts_with("anthropic.claude") {
            return Some(trimmed);
        }
        None
    }

    fn azure_model_id_from_name(model_name: &str) -> Option<&str> {
        let trimmed = model_name.trim();
        if let Some(model_id) = trimmed.strip_prefix("azure/") {
            return (!model_id.trim().is_empty()).then_some(model_id.trim());
        }
        None
    }

    fn bedrock_model_config(model_id: &str) -> ModelConfig {
        ModelConfig {
            provider: "bedrock".to_string(),
            model: model_id.to_string(),
            temperature: 0.7,
            max_tokens: 4096,
        }
    }

    fn azure_model_config(model_id: &str) -> ModelConfig {
        ModelConfig {
            provider: "azure".to_string(),
            model: model_id.to_string(),
            temperature: 0.7,
            max_tokens: 4096,
        }
    }

    #[allow(dead_code)]
    fn runtime_bedrock_region(provider_name: &str, entry: &RuntimeProviderEntry) -> Result<String> {
        entry
            .headers
            .get("region")
            .cloned()
            .or_else(|| {
                entry
                    .headers
                    .get("region_env")
                    .and_then(|env| std::env::var(env).ok())
            })
            .or_else(|| std::env::var("AWS_REGION").ok())
            .filter(|region| !region.is_empty())
            .ok_or_else(|| {
                AppError::Configuration(format!(
                    "Runtime Bedrock provider '{}' must define headers.region or AWS_REGION",
                    provider_name
                ))
            })
    }

    #[cfg(not(feature = "genai"))]
    fn provider_from_runtime_entry_with_params(
        provider_name: &str,
        entry: &RuntimeProviderEntry,
        model_override: Option<&str>,
        params: ModelParams,
    ) -> Result<Provider> {
        let _ = (model_override, params);
        Err(AppError::Configuration(format!(
            "runtime provider '{}' requires the `genai` feature (provider_type '{}')",
            provider_name, entry.provider_type
        )))
    }

    #[cfg(feature = "genai")]
    #[allow(unused_variables)]
    fn provider_from_runtime_entry_with_params(
        provider_name: &str,
        entry: &RuntimeProviderEntry,
        model_override: Option<&str>,
        params: ModelParams,
    ) -> Result<Provider> {
        let model = model_override
            .map(String::from)
            .or_else(|| entry.default_model.clone())
            .unwrap_or_default();
        match entry.provider_type.as_str() {
            "openai-compatible" | "custom" => Ok(Provider::from_runtime_openai(
                Self::runtime_api_key(provider_name, entry)?,
                entry.api_base.clone(),
                model,
                params,
                entry.headers.clone(),
            )),
            "anthropic-compatible" => Ok(Provider::Genai(GenaiProvider {
                kind: AdapterKind::Anthropic,
                api_key: Some(Self::runtime_api_key(provider_name, entry)?),
                endpoint: if entry.api_base.is_empty() {
                    None
                } else {
                    Some(entry.api_base.clone())
                },
                model,
                params,
                headers: entry.headers.clone(),
                region: None,
                vertex_project: None,
                vertex_location: None,
                custom_index: None,
            })),
            "bedrock" | "bedrock-compatible" => Ok(Provider::from_runtime_bedrock(
                Self::runtime_api_key(provider_name, entry)?,
                Self::runtime_bedrock_region(provider_name, entry)?,
                model,
                params,
            )),
            "azure" | "azure-compatible" => {
                let api_key = Self::runtime_api_key(provider_name, entry)?;
                Ok(Provider::from_runtime_openai(
                    api_key.clone(),
                    crate::client::azure_normalize_base_url(&entry.api_base),
                    crate::client::azure_strip_model_prefix(&model).to_string(),
                    params,
                    crate::client::azure_foundry_headers(&api_key),
                ))
            }
            provider_type => {
                let kind_key = match provider_type {
                    "openrouter" => "open_router",
                    "github" => "github_copilot",
                    other => other,
                };
                let kind = AdapterKind::from_lower_str(kind_key).ok_or_else(|| {
                    AppError::Configuration(format!(
                        "Runtime provider '{}' has unsupported provider_type '{}'",
                        provider_name, provider_type
                    ))
                })?;
                let endpoint = if entry.api_base.is_empty() {
                    None
                } else {
                    Some(entry.api_base.clone())
                };
                Ok(Provider::Genai(GenaiProvider {
                    kind,
                    api_key: Some(Self::runtime_api_key(provider_name, entry)?),
                    endpoint,
                    model,
                    params,
                    headers: entry.headers.clone(),
                    region: None,
                    vertex_project: None,
                    vertex_location: None,
                    custom_index: None,
                }))
            }
        }
    }

    /// Get a model configuration by name.
    /// Checks explicit legacy models first, then falls back to the live catalog.
    pub fn get_model(&self, name: &str) -> Option<ModelConfig> {
        // 1. explicit legacy models
        if let Some(cfg) = self.models.get(name) {
            return Some(cfg.clone());
        }
        // 2. direct Bedrock model ids (`bedrock/<model-id>` or Bedrock Anthropic ids)
        if let Some(model_id) = Self::bedrock_model_id_from_name(name) {
            return Some(Self::bedrock_model_config(model_id));
        }
        // 3. direct Azure model ids (`azure/<model-id>`)
        if let Some(model_id) = Self::azure_model_id_from_name(name) {
            return Some(Self::azure_model_config(model_id));
        }
        // 4. catalog lookup – synthesize a ModelConfig on the fly
        if let Some(catalog) = &self.catalog {
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
        for (name, entries) in runtime.iter() {
            if entries.iter().any(|entry| entry.tenant_id.is_none()) && !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    /// Get all model names (legacy + catalog ids)
    pub fn model_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.models.keys().cloned().collect();
        if let Some(ProviderConfig::Bedrock { default_model, .. }) = self.get_provider("bedrock") {
            let name = format!("bedrock/{default_model}");
            if !default_model.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        if let Some(ProviderConfig::Azure { default_model, .. }) = self.get_provider("azure") {
            let name = format!("azure/{default_model}");
            if !default_model.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        if let Some(catalog) = &self.catalog {
            for entry in catalog.snapshot() {
                names.push(entry.id.clone());
            }
        }
        names
    }

    /// Create an LLM client for a specific model by name.
    ///
    /// Fleet-wide tenant resolution (tenant `None`).
    pub async fn create_client_for_model(&self, model_name: &str) -> Result<Box<dyn LLMClient>> {
        self.create_client_for_model_inner(model_name, None).await
    }

    /// Create an LLM client for a specific model by name, deriving the tenant
    /// from the context's isolate namespace so tenant-scoped runtime providers
    /// are used when the caller holds a tenant-isolated context.
    pub async fn create_client_for_model_ctx(
        &self,
        ctx: &std::sync::Arc<cordis::Context>,
        model_name: &str,
    ) -> Result<Box<dyn LLMClient>> {
        let tenant = tenant_from_ctx(ctx);
        self.create_client_for_model_inner(model_name, tenant.as_deref())
            .await
    }

    async fn create_client_for_model_inner(
        &self,
        model_name: &str,
        tenant: Option<&str>,
    ) -> Result<Box<dyn LLMClient>> {
        // 1. Try legacy explicit models first
        if let Some(model_config) = self.models.get(model_name) {
            let runtime_entry =
                self.runtime_provider_entry_for_tenant(&model_config.provider, tenant);
            if let Some(entry) = runtime_entry {
                let provider = Self::provider_from_runtime_entry_with_params(
                    &model_config.provider,
                    &entry,
                    Some(&model_config.model),
                    ModelParams::from_model_config(model_config),
                )?;
                return provider.create_client().await;
            }

            let provider_config = self
                .providers
                .get(&model_config.provider)
                .cloned()
                .ok_or_else(|| {
                    AppError::Configuration(format!(
                        "Provider '{}' referenced by model '{}' not found",
                        model_config.provider, model_name
                    ))
                })?;
            let provider = Provider::from_model_config(model_config, &provider_config)?;
            return provider.create_client().await;
        }

        // 2. Try direct Bedrock model routing (`bedrock/<model-id>`).
        if let Some(model_id) = Self::bedrock_model_id_from_name(model_name) {
            let model_config = Self::bedrock_model_config(model_id);
            let provider_config = self
                .provider_for_tenant("bedrock", tenant)
                .unwrap_or_else(Self::default_bedrock_provider_config);
            let provider = Provider::from_model_config(&model_config, &provider_config)?;
            return provider.create_client().await;
        }

        // 3. Try direct Azure model routing (`azure/<model-id>`).
        if let Some(model_id) = Self::azure_model_id_from_name(model_name) {
            let model_config = Self::azure_model_config(model_id);
            let provider_config = self
                .provider_for_tenant("azure", tenant)
                .unwrap_or_else(Self::default_azure_provider_config);
            let provider = Provider::from_model_config(&model_config, &provider_config)?;
            return provider.create_client().await;
        }

        // 4. Try catalog lookup
        if let Some(catalog) = &self.catalog {
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
        // Check runtime providers first so resolved API keys and custom headers are preserved.
        let runtime_entry = self.runtime_provider_entry_for_tenant(provider_name, None);
        if let Some(entry) = runtime_entry {
            let provider = Self::provider_from_runtime_entry(provider_name, &entry)?;
            return provider.create_client().await;
        }

        let provider_config = self.providers.get(provider_name).ok_or_else(|| {
            AppError::Configuration(format!(
                "Provider '{}' not found in configuration",
                provider_name
            ))
        })?;

        let provider = Provider::from_config(provider_config, None)?;
        provider.create_client().await
    }

    /// Create an LLM client for an already-resolved provider/model pair.
    pub async fn create_client_for_resolved_provider(
        &self,
        resolved: &ResolvedProviderConfig,
    ) -> Result<Box<dyn LLMClient>> {
        let runtime_entry = self.runtime_provider_entry_for_tenant(
            &resolved.provider_name,
            resolved.tenant_id.as_deref(),
        );
        if let Some(entry) = runtime_entry {
            let provider = Self::provider_from_runtime_entry_with_params(
                &resolved.provider_name,
                &entry,
                Some(&resolved.model_name),
                resolved.params.clone(),
            )?;
            return provider.create_client().await;
        }

        let provider = Provider::from_config_with_params(
            &resolved.provider_config,
            Some(&resolved.model_name),
            resolved.params.clone(),
        )?;
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
            || Self::bedrock_model_id_from_name(name).is_some()
            || Self::azure_model_id_from_name(name).is_some()
            || self
                .catalog
                .as_ref()
                .map(|c| c.snapshot().iter().any(|e| e.id == name))
                .unwrap_or(false)
    }

    /// Check if a provider exists in the registry (legacy or runtime).
    pub fn has_provider(&self, name: &str) -> bool {
        self.has_provider_for_tenant(name, None)
    }

    pub fn has_provider_for_tenant(&self, name: &str, tenant_id: Option<&str>) -> bool {
        self.providers.contains_key(name)
            || self
                .runtime_provider_entry_for_tenant(name, tenant_id)
                .is_some()
    }

    // ================== Capability-Based Model Selection (DIR-43) ==================

    /// Get capabilities for a registered model.
    pub fn get_model_capabilities(&self, model_name: &str) -> Option<ModelCapabilities> {
        if let Some(model_id) = Self::bedrock_model_id_from_name(model_name) {
            let mut caps = ModelCapabilities::for_model(model_id);
            caps.is_local = false;
            return Some(caps);
        }
        if let Some(model_id) = Self::azure_model_id_from_name(model_name) {
            let mut caps = ModelCapabilities::for_model(model_id);
            caps.is_local = false;
            return Some(caps);
        }

        // If it's a legacy model, use the explicit config
        if let Some(model_config) = self.models.get(model_name) {
            let provider_config = self.get_provider(&model_config.provider)?;
            let mut caps = ModelCapabilities::for_model(&model_config.model);
            if matches!(
                provider_config,
                ProviderConfig::OpenAI { .. }
                    | ProviderConfig::Azure { .. }
                    | ProviderConfig::Bedrock { .. }
            ) {
                caps.is_local = false;
            }
            return Some(caps);
        }

        // If it's in the catalog, use the catalog id directly
        if let Some(catalog) = &self.catalog {
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

        if let Some(ProviderConfig::Bedrock { default_model, .. }) = self.get_provider("bedrock") {
            let name = format!("bedrock/{default_model}");
            if !default_model.is_empty() && !result.iter().any(|model| model.name == name) {
                let mut caps = ModelCapabilities::for_model(&default_model);
                caps.is_local = false;
                result.push(ModelWithCapabilities {
                    name,
                    provider: "bedrock".to_string(),
                    model_id: default_model,
                    capabilities: caps,
                });
            }
        }

        if let Some(ProviderConfig::Azure { default_model, .. }) = self.get_provider("azure") {
            let name = format!("azure/{default_model}");
            if !default_model.is_empty() && !result.iter().any(|model| model.name == name) {
                let mut caps = ModelCapabilities::for_model(&default_model);
                caps.is_local = false;
                result.push(ModelWithCapabilities {
                    name,
                    provider: "azure".to_string(),
                    model_id: default_model,
                    capabilities: caps,
                });
            }
        }

        // Catalog models
        if let Some(catalog) = &self.catalog {
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
            let capabilities = ModelCapabilities::for_model(&config.model);
            models.push(ModelInfo {
                name: name.clone(),
                provider: config.provider.clone(),
                model: config.model.clone(),
                owned_by: config.provider.clone(),
                quality_score: 75,
                is_chat: true,
                supports_reasoning: capabilities.supports_reasoning,
                supports_streaming: capabilities.supports_streaming,
            });
        }

        if let Some(ProviderConfig::Bedrock { default_model, .. }) = self.get_provider("bedrock") {
            let name = format!("bedrock/{default_model}");
            if !default_model.is_empty() && !models.iter().any(|model| model.name == name) {
                let capabilities = ModelCapabilities::for_model(&default_model);
                models.push(ModelInfo {
                    name,
                    provider: "bedrock".to_string(),
                    model: default_model,
                    owned_by: "aws-bedrock".to_string(),
                    quality_score: 85,
                    is_chat: true,
                    supports_reasoning: capabilities.supports_reasoning,
                    supports_streaming: capabilities.supports_streaming,
                });
            }
        }

        if let Some(ProviderConfig::Azure { default_model, .. }) = self.get_provider("azure") {
            let name = format!("azure/{default_model}");
            if !default_model.is_empty() && !models.iter().any(|model| model.name == name) {
                let capabilities = ModelCapabilities::for_model(&default_model);
                models.push(ModelInfo {
                    name,
                    provider: "azure".to_string(),
                    model: default_model,
                    owned_by: "azure-foundry".to_string(),
                    quality_score: 80,
                    is_chat: true,
                    supports_reasoning: capabilities.supports_reasoning,
                    supports_streaming: capabilities.supports_streaming,
                });
            }
        }

        // Catalog entries
        if let Some(catalog) = &self.catalog {
            let snapshot = catalog.snapshot();
            if !snapshot.is_empty() {
                for entry in snapshot {
                    let capabilities = self
                        .get_model_capabilities(&entry.id)
                        .unwrap_or_else(|| ModelCapabilities::for_model(&entry.id));
                    models.push(ModelInfo {
                        name: entry.id.clone(),
                        provider: "nvidia".to_string(),
                        model: entry.id.clone(),
                        owned_by: entry.owned_by.clone(),
                        quality_score: entry.quality_score,
                        is_chat: true,
                        supports_reasoning: capabilities.supports_reasoning,
                        supports_streaming: capabilities.supports_streaming,
                    });
                }
            } else if let Some(default) = &self.default_model {
                // Fallback when catalog is empty: expose the default model so the UI is never blank
                let capabilities = ModelCapabilities::for_model(default);
                models.push(ModelInfo {
                    name: default.clone(),
                    provider: "nvidia".to_string(),
                    model: default.clone(),
                    owned_by: "unknown".to_string(),
                    quality_score: 75,
                    is_chat: true,
                    supports_reasoning: capabilities.supports_reasoning,
                    supports_streaming: capabilities.supports_streaming,
                });
            }
        } else if let Some(default) = &self.default_model {
            // No catalog at all – still expose the default model
            let capabilities = ModelCapabilities::for_model(default);
            models.push(ModelInfo {
                name: default.clone(),
                provider: "nvidia".to_string(),
                model: default.clone(),
                owned_by: "unknown".to_string(),
                quality_score: 75,
                is_chat: true,
                supports_reasoning: capabilities.supports_reasoning,
                supports_streaming: capabilities.supports_streaming,
            });
        }

        models
    }

    /// Capability-aware fallback chain for `Llm`.
    ///
    /// Tries `create_client_for_requirements` for the given `requirements` if
    /// `Some`, then falls back to `create_default_client`. This reuses the
    /// existing `find_best_model` → `create_client_for_model` chain and the
    /// coordinator's fallback semantics without requiring a database.
    pub async fn resolve_with_capability_fallback(
        &self,
        requirements: Option<CapabilityRequirements>,
    ) -> Result<Box<dyn crate::client::LLMClient>> {
        if let Some(req) = requirements {
            if let Ok(client) = self.create_client_for_requirements(&req).await {
                return Ok(client);
            }
        }
        self.create_default_client().await
    }

    /// Alias satisfying the `Llm` spec's `resolve_with_fallback` name
    /// when the postgres-gated tier resolver is not active.
    #[cfg(not(feature = "postgres"))]
    pub async fn resolve_with_fallback(
        &self,
        requirements: Option<CapabilityRequirements>,
    ) -> Result<Box<dyn crate::client::LLMClient>> {
        self.resolve_with_capability_fallback(requirements).await
    }

    // ============================================================
    // Helpers
    // ============================================================

    /// Resolve a model tier or model name to concrete provider/model entries,
    /// following the fallback chain stored in `fleet_secrets` for the primary provider.
    ///
    /// 1. Looks up `tenant_model_tiers` for the tenant + tier.
    /// 2. Falls back to the registry's configured models.
    /// 3. Falls back to treating `tier_or_model` as a provider name.
    /// 4. Loads the primary provider's `fallback_providers` from fleet secrets
    ///    and appends each resolved provider with its own concrete default model.
    #[cfg(feature = "postgres")]
    pub async fn resolve_with_fallback(
        &self,
        tier_or_model: &str,
        tenant_id: &str,
        pool: &sqlx::PgPool,
        fleet_secrets: &ares_store::FleetSecrets,
    ) -> Result<Vec<ResolvedProviderConfig>> {
        use ares_store::tenant_model_tiers::TenantModelTierStore;
        use std::collections::HashSet;

        let store = TenantModelTierStore::new(pool);
        let primary = match store.get(tenant_id, tier_or_model).await {
            Ok(Some(tier)) => {
                let provider_config = self
                    .provider_for_tenant(&tier.provider_name, Some(tenant_id))
                    .ok_or_else(|| {
                        AppError::Configuration(format!(
                            "Provider '{}' configured for tenant '{}' tier '{}' not found",
                            tier.provider_name, tenant_id, tier_or_model
                        ))
                    })?;
                ResolvedProviderConfig {
                    provider_name: tier.provider_name,
                    model_name: tier.model_name,
                    provider_config,
                    params: ModelParams::default(),
                    tenant_id: Some(tenant_id.to_string()),
                }
            }
            Ok(None) => {
                if let Some(model_cfg) = self.get_model(tier_or_model) {
                    let provider_config = self
                        .provider_for_tenant(&model_cfg.provider, Some(tenant_id))
                        .ok_or_else(|| {
                            AppError::Configuration(format!(
                                "Provider '{}' referenced by model/tier '{}' not found",
                                model_cfg.provider, tier_or_model
                            ))
                        })?;
                    ResolvedProviderConfig {
                        provider_name: model_cfg.provider.clone(),
                        model_name: model_cfg.model.clone(),
                        provider_config,
                        params: ModelParams::from_model_config(&model_cfg),
                        tenant_id: Some(tenant_id.to_string()),
                    }
                } else if let Some(provider_config) =
                    self.provider_for_tenant(tier_or_model, Some(tenant_id))
                {
                    let model_name = Self::provider_default_model(&provider_config).to_string();
                    if model_name.is_empty() {
                        return Err(AppError::Configuration(format!(
                            "Provider '{}' has no concrete default model configured",
                            tier_or_model
                        )));
                    }
                    ResolvedProviderConfig {
                        provider_name: tier_or_model.to_string(),
                        model_name,
                        provider_config,
                        params: ModelParams::default(),
                        tenant_id: Some(tenant_id.to_string()),
                    }
                } else {
                    return Err(AppError::Configuration(format!(
                        "No provider or model/tier '{}' found for tenant '{}'",
                        tier_or_model, tenant_id
                    )));
                }
            }
            Err(e) => {
                return Err(AppError::Database(format!(
                    "Failed to resolve tenant '{}' model tier '{}': {}",
                    tenant_id, tier_or_model, e
                )));
            }
        };

        let primary_provider = primary.provider_name.clone();
        let mut result = vec![primary];
        let mut seen = HashSet::new();
        seen.insert(primary_provider.clone());

        if let Some(override_) = fleet_secrets.get(&primary_provider) {
            for fallback_name in &override_.fallback_providers {
                if seen.contains(fallback_name) {
                    continue;
                }
                let provider_config = self
                    .provider_for_tenant(fallback_name, Some(tenant_id))
                    .ok_or_else(|| {
                        AppError::Configuration(format!(
                            "Fallback provider '{}' configured for primary provider '{}' not found",
                            fallback_name, primary_provider
                        ))
                    })?;
                let model_name = Self::provider_default_model(&provider_config).to_string();
                if model_name.is_empty() {
                    return Err(AppError::Configuration(format!(
                        "Fallback provider '{}' configured for primary provider '{}' has no concrete default model configured",
                        fallback_name, primary_provider
                    )));
                }
                seen.insert(fallback_name.clone());
                result.push(ResolvedProviderConfig {
                    provider_name: fallback_name.clone(),
                    model_name,
                    provider_config,
                    params: ModelParams::default(),
                    tenant_id: Some(tenant_id.to_string()),
                });
            }
        }

        Ok(result)
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
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default)]
    pub supports_streaming: bool,
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
    pub fn from_config(
        providers: std::collections::HashMap<String, ProviderConfig>,
        models: std::collections::HashMap<String, ModelConfig>,
        nvidia: Option<&NvidiaConfig>,
    ) -> Result<Self> {
        let registry = ProviderRegistry::from_config(providers, models.clone(), nvidia);

        let default_model = nvidia
            .map(|n| n.default_model.clone())
            .or_else(|| models.keys().next().cloned())
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

// REMOVED: polling fallback retained for one release then delete. Unified hot-reload now via ReflectService::notify(TypeId::of::<ProviderRegistry>()) BFS walks dependents and calls Fiber::refresh via watch channel.

/// Phase 3 unified hot-reload demo — watch channel creation on provide.
/// Compile-time proof that notifiers/dependents insertion compiles via ReflectService.
pub fn reflect_notify_stub(ctx: &Arc<cordis::Context>) {
    // Prove Loader integration still compiles
    let _ = ctx.get::<cordis::loader::Loader>();
    let tid = TypeId::of::<ProviderRegistry>();
    // Prove ReflectService watch channel creation on provide + dependents insertion + BFS notify compiles
    if let Some(reflect) = ctx.get::<ReflectService>() {
        let _rx = reflect.ensure_notifier(tid);
        reflect.register_dependent(tid, 43);
        reflect.notify(tid);
    }
    let _ = tid;
}

/// Derive the tenant id from the context's isolate namespace for [`Llm`],
/// stripping a leading `tenant:`/`user:` prefix.
///
/// Isolate labels win. When unlabeled for `Llm`, falls back to a
/// [`ares_types::models::TenantContext`] intercept (`tenant_id` if non-empty).
/// Empty labels/ids yield `None` (fleet-wide resolution).
pub(crate) fn tenant_from_ctx(ctx: &std::sync::Arc<cordis::Context>) -> Option<String> {
    ctx.isolate_label(std::any::TypeId::of::<crate::Llm>())
        .and_then(|label| {
            label
                .strip_prefix("tenant:")
                .or_else(|| label.strip_prefix("user:"))
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            ctx.get::<ares_types::models::TenantContext>()
                .map(|tc| tc.tenant_id.clone())
                .filter(|s| !s.is_empty())
        })
}

// Cordis Service impl — allows ctx.get::<ProviderRegistry>() for crate wiring.
// Per-tenant provider-secret isolate labels key on TypeId::of::<Llm>(), not this type.
impl cordis::Service for ProviderRegistry {
    fn name(&self) -> &'static str {
        "provider_registry"
    }
    fn init(&self, _ctx: &std::sync::Arc<cordis::Context>) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

// Cordis Service impl — allows direct ctx.get::<ConfigBasedLLMFactory>() without wrapper
impl cordis::Service for ConfigBasedLLMFactory {
    fn name(&self) -> &'static str {
        "llm_factory"
    }
    fn init(&self, _ctx: &std::sync::Arc<cordis::Context>) -> cordis::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::CapabilityRequirements;

    use crate::config::{ModelConfig, ProviderConfig};
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

    fn from_maps(
        providers: HashMap<String, ProviderConfig>,
        models: HashMap<String, ModelConfig>,
    ) -> crate::provider_registry::ProviderRegistry {
        ProviderRegistry::from_config(providers, models, None)
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
            Ok(_) => {
                panic!("expected Configuration error containing {expected_substring:?}, got Ok")
            }
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

    #[cfg(feature = "genai")]
    #[test]
    fn test_runtime_openai_provider_preserves_key_and_headers() {
        let mut headers = HashMap::new();
        headers.insert("X-Test-Header".to_string(), "runtime-value".to_string());
        let entry = RuntimeProviderEntry {
            tenant_id: None,
            display_name: "Runtime OpenAI".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://runtime.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("runtime-model".to_string()),
            headers,
            api_key: Some("resolved-runtime-key".to_string()),
            enabled: true,
        };

        let provider = ProviderRegistry::provider_from_runtime_entry("runtime-openai", &entry)
            .expect("runtime provider should resolve");
        match provider {
            Provider::Genai(g) => {
                assert_eq!(g.api_key.as_deref(), Some("resolved-runtime-key"));
                assert_eq!(g.endpoint.as_deref(), Some("https://runtime.example.com/v1"));
                assert_eq!(g.model, "runtime-model");
                assert_eq!(
                    g.headers.get("X-Test-Header").map(String::as_str),
                    Some("runtime-value")
                );
            }
            _ => panic!("expected Genai provider"),
        }
    }

    #[cfg(feature = "genai")]
    #[test]
    fn test_runtime_provider_requires_resolved_api_key() {
        let entry = RuntimeProviderEntry {
            tenant_id: None,
            display_name: "Runtime OpenAI".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://runtime.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("runtime-model".to_string()),
            headers: HashMap::new(),
            api_key: None,
            enabled: true,
        };

        assert_configuration_error(
            ProviderRegistry::provider_from_runtime_entry("runtime-openai", &entry),
            "Runtime provider 'runtime-openai' API key is not resolved",
        );
    }

    #[test]
    fn runtime_provider_visibility_respects_tenant_scope() {
        let registry = ProviderRegistry::new();
        let global = RuntimeProviderEntry {
            tenant_id: None,
            display_name: "Global Runtime".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://global.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("global-model".to_string()),
            headers: HashMap::new(),
            api_key: Some("global-key".to_string()),
            enabled: true,
        };
        let scoped = RuntimeProviderEntry {
            tenant_id: Some("tenant-a".to_string()),
            display_name: "Scoped Runtime".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://tenant.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("tenant-model".to_string()),
            headers: HashMap::new(),
            api_key: Some("tenant-key".to_string()),
            enabled: true,
        };
        registry.reload_runtime_providers(
            vec![global, scoped],
            vec!["global-runtime".to_string(), "tenant-runtime".to_string()],
        );

        assert!(registry.has_provider("global-runtime"));
        assert!(!registry.has_provider("tenant-runtime"));
        assert!(registry.has_provider_for_tenant("tenant-runtime", Some("tenant-a")));
        assert!(!registry.has_provider_for_tenant("tenant-runtime", Some("tenant-b")));
        assert!(
            registry
                .provider_for_tenant("tenant-runtime", Some("tenant-a"))
                .is_some()
        );
        assert!(
            registry
                .provider_for_tenant("tenant-runtime", Some("tenant-b"))
                .is_none()
        );
        assert_eq!(
            registry.provider_names(),
            vec!["global-runtime".to_string()]
        );
    }

    #[test]
    fn runtime_provider_lookup_prefers_tenant_override_same_name() {
        let registry = ProviderRegistry::new();
        let global = RuntimeProviderEntry {
            tenant_id: None,
            display_name: "Global Shared".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://global.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("global-model".to_string()),
            headers: HashMap::new(),
            api_key: Some("global-key".to_string()),
            enabled: true,
        };
        let tenant = RuntimeProviderEntry {
            tenant_id: Some("tenant-a".to_string()),
            display_name: "Tenant Shared".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://tenant.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("tenant-model".to_string()),
            headers: HashMap::new(),
            api_key: Some("tenant-key".to_string()),
            enabled: true,
        };
        registry.reload_runtime_providers(
            vec![global, tenant],
            vec!["shared-runtime".to_string(), "shared-runtime".to_string()],
        );

        assert!(registry.has_provider("shared-runtime"));
        assert!(registry.has_provider_for_tenant("shared-runtime", Some("tenant-a")));
        assert!(registry.has_provider_for_tenant("shared-runtime", Some("tenant-b")));

        let tenant_provider = registry
            .provider_for_tenant("shared-runtime", Some("tenant-a"))
            .expect("tenant provider");
        let global_provider = registry
            .provider_for_tenant("shared-runtime", Some("tenant-b"))
            .expect("global fallback provider");
        assert_eq!(
            ProviderRegistry::provider_default_model(&tenant_provider),
            "tenant-model"
        );
        assert_eq!(
            ProviderRegistry::provider_default_model(&global_provider),
            "global-model"
        );
        assert_eq!(
            registry.provider_names(),
            vec!["shared-runtime".to_string()]
        );
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
        let fast = models
            .iter()
            .find(|m| m.name == "fast-local" && m.provider == "nvidia")
            .expect("fast model");
        assert!(fast.supports_reasoning);
        assert!(fast.supports_streaming);
    }

    #[test]
    fn test_from_config_loads_providers_and_models() {
        let mut providers = HashMap::new();
        providers.insert("nvidia".to_string(), sample_openai_provider());
        let mut models = HashMap::new();
        models.insert(
            "fast".to_string(),
            sample_model_config("nvidia", "test-model"),
        );

        let registry = from_maps(providers, models);

        assert!(registry.has_provider("nvidia"));
        assert!(registry.has_model("fast"));

        let names = registry.model_names();
        assert!(names.contains(&"fast".to_string()));
        assert!(names.iter().any(|n| n.starts_with("bedrock/")));
        assert!(names.iter().any(|n| n.starts_with("azure/")));
    }

    #[tokio::test]
    async fn test_set_default_model() {
        let mut registry = create_test_registry();
        registry.set_default_model("powerful-local");
        // The fixture's provider reads TEST_KEY; other suites export it, so
        // clear it here to keep the missing-key error deterministic.
        let saved_key = std::env::var("TEST_KEY").ok();
        std::env::remove_var("TEST_KEY");
        let result = registry.create_default_client().await;
        if let Some(key) = saved_key {
            std::env::set_var("TEST_KEY", key);
        }
        match result {
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
        models.insert(
            "fast".to_string(),
            sample_model_config("nvidia", "test-model"),
        );

        let factory = ConfigBasedLLMFactory::from_config(providers, models, None).unwrap();
        assert_eq!(factory.default_model(), "fast");
        assert!(factory.registry().has_model("fast"));
    }

    #[test]
    fn test_config_factory_from_config_no_models() {
        let factory =
            ConfigBasedLLMFactory::from_config(HashMap::new(), HashMap::new(), None).unwrap();
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

    #[test]
    fn get_provider_for_ctx_derives_tenant_from_isolate_namespace() {
        // Cordis design (§4): per-tenant provider resolution should be driven by
        // the context's isolate namespace, not a throwaway method param.
        // get_provider_for_ctx(ctx, name) reads
        // ctx.isolate_label(TypeId::of::<Llm>()) and strips a
        // leading 'tenant:' prefix (mirroring resolver::user_id_from_ctx) so a
        // tenant-isolated context resolves the tenant-scoped provider.

        let registry = ProviderRegistry::new();
        let global = RuntimeProviderEntry {
            tenant_id: None,
            display_name: "Global Shared".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://global.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("global-model".to_string()),
            headers: HashMap::new(),
            api_key: Some("global-key".to_string()),
            enabled: true,
        };
        let tenant = RuntimeProviderEntry {
            tenant_id: Some("tenant-a".to_string()),
            display_name: "Tenant Shared".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://tenant.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("tenant-model".to_string()),
            headers: HashMap::new(),
            api_key: Some("tenant-key".to_string()),
            enabled: true,
        };
        registry.reload_runtime_providers(
            vec![global, tenant],
            vec!["shared-runtime".to_string(), "shared-runtime".to_string()],
        );

        let ctx: Arc<cordis::Context> = cordis::Context::new_root();

        // Untagged context -> no isolate label -> falls back to the fleet-wide
        // provider (the shared "shared-runtime" entry's tenant is None).
        let fleet = registry.get_provider_for_ctx(&ctx, "shared-runtime");
        assert!(
            fleet.is_some(),
            "untagged ctx should resolve the fleet provider"
        );

        // A tenant-isolated context must drive resolution: the isolate label
        // 'tenant:tenant-a' derives tenant 'tenant-a', so the tenant-scoped
        // provider wins.
        let tenant_ctx = ctx.isolate::<crate::Llm>("tenant:tenant-a");
        let tenant_provider = registry.get_provider_for_ctx(&tenant_ctx, "shared-runtime");
        assert!(
            tenant_provider.is_some(),
            "tenant:tenant-a isolated ctx should resolve a provider"
        );
    }

    #[test]
    fn tenant_from_ctx_reads_tenant_context_intercept() {
        // Unlabeled root has no isolate label and no intercept, so tenant is
        // None (fleet-wide). A TenantContext intercept is the fallback when
        // isolate_label is missing.
        let ctx: Arc<cordis::Context> = cordis::Context::new_root();
        assert_eq!(tenant_from_ctx(&ctx), None);

        let intercepted = ctx.with_intercept(ares_types::models::TenantContext::new(
            "tenant-a".into(),
            ares_types::models::TenantTier::Pro,
        ));
        assert_eq!(tenant_from_ctx(&intercepted), Some("tenant-a".to_string()));

        // Intercept-only ctx should resolve the tenant-a runtime provider.
        let registry = ProviderRegistry::new();
        let global = RuntimeProviderEntry {
            tenant_id: None,
            display_name: "Global Shared".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://global.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("global-model".to_string()),
            headers: HashMap::new(),
            api_key: Some("global-key".to_string()),
            enabled: true,
        };
        let tenant = RuntimeProviderEntry {
            tenant_id: Some("tenant-a".to_string()),
            display_name: "Tenant Shared".to_string(),
            provider_type: "openai-compatible".to_string(),
            api_base: "https://tenant.example.com/v1".to_string(),
            auth_type: "api_key".to_string(),
            default_model: Some("tenant-model".to_string()),
            headers: HashMap::new(),
            api_key: Some("tenant-key".to_string()),
            enabled: true,
        };
        registry.reload_runtime_providers(
            vec![global, tenant],
            vec!["shared-runtime".to_string(), "shared-runtime".to_string()],
        );
        let provider = registry
            .get_provider_for_ctx(&intercepted, "shared-runtime")
            .expect("intercept-only ctx should resolve the tenant-a provider");
        assert_eq!(
            ProviderRegistry::provider_default_model(&provider),
            "tenant-model"
        );
    }

    #[test]
    fn tenant_from_ctx_isolate_label_wins_over_intercept() {
        // Isolate label is the primary source even when a TenantContext
        // intercept is present. Intercept "from-intercept", then isolate
        // as tenant:from-isolate, must yield the isolate id.
        let ctx: Arc<cordis::Context> = cordis::Context::new_root();
        let intercepted = ctx.with_intercept(ares_types::models::TenantContext::new(
            "from-intercept".into(),
            ares_types::models::TenantTier::Pro,
        ));
        let isolated = intercepted.isolate::<crate::Llm>("tenant:from-isolate");
        assert_eq!(tenant_from_ctx(&isolated), Some("from-isolate".to_string()));
    }
}
