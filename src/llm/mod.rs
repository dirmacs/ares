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
//! - [`ToolCoordinator`](crate::llm::coordinator::ToolCoordinator) - Generic multi-turn tool calling coordinator
//!
//! # Supported Providers
//!
//! Enable providers via Cargo features:
//! - `openai` - OpenAI API (GPT-4, GPT-3.5, etc.)
//! - `anthropic` - Anthropic API (Claude 3, Claude 3.5, etc.)
//! - `ollama` - Local Ollama server
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
//! # Tool Calling
//!
//! Use the [`ToolCoordinator`](crate::llm::coordinator::ToolCoordinator) for multi-turn tool calling with any provider:
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

/// Core LLM client trait and streaming response types.
pub mod client;
/// Generic tool coordinator for multi-turn tool calling.
pub mod coordinator;
/// Registry for managing multiple LLM provider instances.
pub mod provider_registry;

#[cfg(feature = "llamacpp")]
pub mod llamacpp;

#[cfg(feature = "ollama")]
pub mod ollama;

#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "anthropic")]
pub mod anthropic;

pub use client::{LLMClient, LLMClientFactory, LLMResponse, Provider};
pub use coordinator::{
    ConversationMessage, CoordinatorResult, FinishReason, MessageRole, ToolCallRecord,
    ToolCallingConfig, ToolCoordinator,
};
pub use provider_registry::{ConfigBasedLLMFactory, ProviderRegistry};
