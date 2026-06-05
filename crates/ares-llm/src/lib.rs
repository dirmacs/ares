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
//! Enable providers via Cargo features:
//! - `openai` - OpenAI API (GPT-4, GPT-3.5, etc.)
//! - `llamacpp` - llama.cpp server
//!
//! # Example
//!
//! ```ignore
//! use ares::llm::{ConfigBasedLLMFactory, LLMClientFactory, Provider};
//!
//! let factory = ConfigBasedLLMFactory::new(&config);
//! let client = factory.create_client(Provider::OpenAI)?;
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
//! let coordinator = ToolCoordinator::new(client, registry, ToolCallingConfig::default());
//! let result = coordinator.execute(Some("System prompt"), "User query").await?;
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
/// Generic tool coordinator for multi-turn tool calling.
pub mod coordinator;
/// Connection pooling for LLM clients (DIR-44).
pub mod pool;
/// Registry for managing multiple LLM provider instances.
pub mod provider_registry;

#[cfg(feature = "openai")]
pub mod openai;

pub use capabilities::{
    CapabilityRequirements, CapabilityRequirementsBuilder, ModelCapabilities, ModelWithCapabilities,
};
pub use client::{LLMClient, LLMClientFactory, LLMResponse, Provider};
pub use coordinator::{
    ConversationMessage, CoordinatorResult, FinishReason, MessageRole, ToolCallRecord,
    ToolCallingConfig, ToolCoordinator,
};
pub use pool::{ClientPool, ClientPoolBuilder, PoolConfig, PoolStats, PooledClientGuard};
pub use provider_registry::{ConfigBasedLLMFactory, ProviderRegistry};

// Re-export NVIDIA catalog types from ares-config so callers can construct caches.
pub use ares_config::nvidia_catalog::{CatalogEntry, NvidiaCatalogCache, NvidiaConfig};

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
