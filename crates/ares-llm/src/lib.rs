//! LLM Provider Clients and Abstractions
//!
//! This module provides a unified interface for interacting with various Large Language
//! Model (LLM) providers. It abstracts away provider-specific implementations behind
//! common traits, allowing the rest of the application to work with any supported LLM.
//!
//! # Architecture
//!
//! The module follows a factory pattern:
//! - [`LLMClient`] - The core trait that all providers implement
//! - [`LLMClientFactory`] - Factory trait for creating provider clients
//! - [`ProviderRegistry`] - Registry for managing multiple providers
//! - [`ConfigBasedLLMFactory`] - Creates clients based on `ares.toml` configuration
//! - [`ToolCoordinator`](crate::coordinator::ToolCoordinator) - Generic multi-turn tool calling coordinator
//! - [`ClientPool`](crate::pool::ClientPool) - Connection pooling for efficient client reuse (DIR-44)
//!
//! # Supported Providers
//!
//! HTTP providers are routed through `genai` (default). Optional:
//! - `llamacpp` - llama.cpp GGUF inference
//!
//! # Example
//!
//! ```ignore
//! use ares::llm::{ConfigBasedLLMFactory, LLMClientFactory, Provider};
//!
//! let factory = ConfigBasedLLMFactory::new(&config);
//! let client = factory.create_default().await?;
//!
//! let response = client.generate("What is 2+2?", None).await?;
//! println!("{}", response.content);
//! ```
//!
//! # Connection Pooling (DIR-44)
//!
//! Use the [`ClientPool`](crate::pool::ClientPool) for efficient connection reuse:
//!
//! ```ignore
//! use ares::llm::pool::{ClientPool, PoolConfig};
//!
//! let pool = ClientPool::new(PoolConfig::default());
//! pool.register_provider("openai", provider);
//!
//! // Get a pooled client - automatically returned when guard is dropped
//! let guard = pool.get("openai").await?;
//! let response = guard.generate("Hello!").await?;
//! ```
//!
//! # Tool Calling
//!
//! Use the [`ToolCoordinator`](crate::coordinator::ToolCoordinator) for multi-turn tool calling with any provider:
//!
//! ```ignore
//! use ares::llm::coordinator::{ToolCoordinator, ToolCallingConfig};
//!
//! let tools = std::sync::Arc::new(ares_tools::Tools::from_static(
//!     Vec::<std::sync::Arc<dyn ares_tools::Tool>>::new(),
//! ));
//! let coordinator = ToolCoordinator::new(client, tools, ToolCallingConfig::default());
//! let ctx = cordis::Context::new_root();
//! let result = coordinator.execute(Some("System prompt"), "User query", &ctx).await?;
//! ```
//!
//! # Streaming
//!
//! All providers support streaming responses via the `generate_stream` method,
//! which returns a `Pin<Box<dyn Stream<Item = Result<String>>>>`.

/// Model capabilities and requirement matching (DIR-43).
pub mod capabilities;
/// Core LLM client trait and streaming response types.
pub mod client;
/// History compaction service: score, audit, critical facts, memory.
pub mod compact;
/// Generic tool coordinator for multi-turn tool calling.
pub mod coordinator;
/// Exporter-style log routing for LLM and tool call records.
pub mod exporter;
/// Per-provider in-flight admission control (`max_in_flight`).
pub mod governor;
/// Unified LLM capability (Cordis Phase 3).
pub mod llm_service;
/// Small-call orchestration primitive over a single client.
pub mod micro;
/// Provider-neutral model profile catalog with lean hints and routing.
pub mod model_catalog;
/// Observability callbacks for LLM and tool call logging.
pub mod observability;
/// Declarative Cordis loader factories for this crate.
pub mod plugins;
/// Connection pooling for LLM clients (DIR-44).
pub mod pool;
/// Registry for managing multiple LLM provider instances.
pub mod provider_registry;

#[cfg(feature = "genai")]
pub mod genai_client;
#[cfg(feature = "llamacpp")]
pub mod llamacpp;

pub use capabilities::{
    CapabilityRequirements, CapabilityRequirementsBuilder, ModelCapabilities, ModelWithCapabilities,
};
#[cfg(feature = "genai")]
pub use client::GenaiProvider;
pub use client::{
    CacheControl, GenerationHints, LLMClient, LLMClientFactory, LLMResponse, LlmStreamEvent,
    Provider, TokenUsage,
};
pub use compact::{
    CompactConfig, CompactEvent, CompactionSnapshot, CompactionState, Compactor, TurnEntry,
};
pub use coordinator::{
    ConversationMessage, CoordinatorResult, FinishReason, MessageRole, ToolCallRecord,
    ToolCallingConfig, ToolCoordinator,
};
pub use exporter::{ExporterRouter, LogExporter, RecordLevel, TracingExporter};
#[cfg(feature = "genai")]
pub use genai::adapter::AdapterKind;
pub use governor::{GovernorConfig, ProviderGovernor};
pub use llm_service::{Breaker, Llm, ModelOverride, TenantModelPolicy};
pub use micro::{MicroCacheConfig, MicroEngine, MicroOutcome, MicroTask};
pub use observability::{LlmCallRecord, ObservabilitySink};
pub use plugins::{install_tracing_router, register_plugins};
pub use pool::{ClientPool, ClientPoolBuilder, PoolConfig, PoolStats, PooledClientGuard};
pub use provider_registry::{ConfigBasedLLMFactory, ProviderRegistry};

pub mod config;
pub mod nvidia_catalog;
pub use config::{ModelConfig, ProviderConfig};
pub use model_catalog::{
    Capability, ModelCatalog, ModelProfile, RouteConstraints, SpeedTier, TaskModality,
};
pub use nvidia_catalog::{CatalogEntry, NvidiaCatalogCache, NvidiaConfig};

#[cfg(test)]
mod lib_tests {
    use super::{CapabilityRequirements, ModelCapabilities, ProviderRegistry};

    #[test]
    fn capability_requirements_builder_defaults() {
        let reqs = CapabilityRequirements::builder().build();
        assert!(reqs.min_context_window.is_none());
    }

    #[test]
    fn model_capabilities_default_context_window() {
        let caps = ModelCapabilities::default();
        assert!(caps.context_window >= 4096);
    }

    #[test]
    fn provider_registry_type_is_constructible() {
        let _ = ProviderRegistry::new();
    }
}
