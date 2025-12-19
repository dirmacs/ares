//! LLM provider clients and abstractions.

#![allow(missing_docs)]

pub mod client;
pub mod provider_registry;

#[cfg(feature = "llamacpp")]
pub mod llamacpp;

#[cfg(feature = "ollama")]
pub mod ollama;

#[cfg(feature = "openai")]
pub mod openai;

pub use client::{LLMClient, LLMClientFactory, LLMResponse, Provider};
pub use provider_registry::{ConfigBasedLLMFactory, ProviderRegistry};
