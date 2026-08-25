//! Cordis loader factories for the LLM capability.
//!
//! Registers a single `"Llm"` factory. Catalog, provider registry, and
//! `ConfigBasedLLMFactory` are composed inside that factory rather than
//! published as standalone loader keys.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use crate::{
    ClientPool, ConfigBasedLLMFactory, Llm, ModelConfig, NvidiaCatalogCache, NvidiaConfig,
    ProviderConfig, ProviderRegistry,
};

fn block_on_plugin<S: cordis::Service + 'static>(
    ctx: &std::sync::Arc<cordis::Context>,
    svc: S,
) -> Result<cordis::FiberId, cordis::CordisError> {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(ctx.plugin(svc)))
}

#[derive(Debug, Default, Deserialize)]
struct LlmPluginConfig {
    #[serde(default)]
    providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    models: HashMap<String, ModelConfig>,
    #[serde(default)]
    nvidia: Option<NvidiaConfig>,
}

fn parse_config(config: &Value) -> LlmPluginConfig {
    match serde_json::from_value::<LlmPluginConfig>(config.clone()) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!("Llm loader config deserialize failed ({err}); using empty defaults");
            LlmPluginConfig::default()
        }
    }
}

fn factory_llm(
    ctx: &Arc<cordis::Context>,
    config: &Value,
) -> Result<cordis::FiberId, cordis::CordisError> {
    let parsed = parse_config(config);
    let nvidia = parsed.nvidia;
    let nvidia_cfg = nvidia.clone().unwrap_or_default();

    let catalog = Arc::new(NvidiaCatalogCache::new(nvidia_cfg.clone()));
    match tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(catalog.refresh())
    }) {
        Ok(count) => tracing::info!("NVIDIA catalog refreshed with {} models", count),
        Err(err) => tracing::warn!("NVIDIA catalog initial refresh failed: {err}"),
    }
    catalog.clone().start_background_refresh();
    tracing::info!(
        "[nvidia] api_base={} default_model={}",
        nvidia_cfg.api_base,
        nvidia_cfg.default_model,
    );

    let registry = ProviderRegistry::from_config(
        parsed.providers.clone(),
        parsed.models.clone(),
        nvidia.as_ref(),
    )
    .with_catalog(catalog.clone());
    let factory =
        ConfigBasedLLMFactory::from_config(parsed.providers, parsed.models, nvidia.as_ref())
            .map_err(|err| cordis::CordisError::Configuration(format!("LLM factory: {err}")))?;
    tracing::info!(
        "LLM factory initialized with default model: {}",
        factory.default_model()
    );

    let pool = Arc::new(ClientPool::with_defaults());
    let llm = Llm::new(Arc::new(registry), pool, Some(catalog)).with_factory(Arc::new(factory));
    block_on_plugin(ctx, llm)
}

/// Register this crate's loader factories. Only `"Llm"` is published
/// (manual fallback path; inventory carries the same key).
pub fn register_plugins(reg: &cordis::PluginRegistry) {
    reg.register("Llm", Arc::new(factory_llm));
}

/// Builds an [`ExporterRouter`] with [`TracingExporter`] registered and
/// returns it ready for fan-out.
///
/// One-line production wiring, e.g. from boot after the Llm plugin loads:
///
/// ```ignore
/// let router = ares_llm::install_tracing_router();
/// router.log_llm_spawned(RecordLevel::Info, record);
/// ```
///
/// Add more sinks later with `Arc::get_mut` before sharing the returned
/// `Arc`, or wrap additional exporters around this call at registration.
pub fn install_tracing_router() -> std::sync::Arc<crate::ExporterRouter> {
    let mut router = crate::ExporterRouter::with_capacity(1);
    if let Err(err) = router.register(std::sync::Arc::new(crate::TracingExporter)) {
        // TracingExporter::validate always succeeds; this is contract armor.
        tracing::warn!("TracingExporter registration failed: {err}");
    }
    Arc::new(router)
}

#[cfg(feature = "inventory")]
inventory::submit! {
    cordis::CordisPluginFactory { name: "Llm", make: factory_llm }
}

#[cfg(test)]
mod tests {
    use super::register_plugins;
    use cordis::PluginRegistry;

    #[test]
    fn register_plugins_registers_only_llm() {
        let reg = PluginRegistry::new();
        register_plugins(&reg);
        let mut names = reg.names();
        names.sort();
        assert_eq!(names, vec!["Llm".to_string()]);
    }
}
