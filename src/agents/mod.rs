//! AI agent orchestration and management.
//!
//! This module provides the agent system for A.R.E.S, including:
//!
//! - **Agent Trait** - Base trait that all agents implement
//! - **ConfigurableAgent** - Dynamic agent created from TOML/TOON configuration
//! - **AgentRegistry** - Registry for creating and managing agent instances
//! - **Router** - Routes requests to appropriate specialized agents
//! - **Orchestrator** - Coordinates multi-step agent workflows
//!
//! ## Architecture
//!
//! All agents are now created dynamically via `ConfigurableAgent`, which reads
//! configuration from TOML files. Legacy hardcoded agents have been removed.
//!
//! ## Example
//!
//! ```rust,ignore
//! use ares::agents::{Agent, AgentRegistry};
//!
//! // Create registry from configuration
//! let registry = AgentRegistry::from_config(&config, provider_registry, tool_registry);
//!
//! // Get an agent instance
//! let agent = registry.get_agent("product")?;
//!
//! // Execute with context
//! let response = agent.execute("Help me with my order", &context).await?;
//! ```

pub mod configurable;
/// Multi-agent orchestration for complex tasks.
pub mod orchestrator;
pub mod registry;
/// Request routing to specialized agents.
pub mod router;

use crate::types::{AgentContext, AgentType, Result};
use async_trait::async_trait;

// Re-export commonly used types
pub use configurable::ConfigurableAgent;
pub use registry::{AgentRegistry, AgentRegistryBuilder};

/// Base trait for all agents
#[async_trait]
pub trait Agent: Send + Sync {
    /// Execute the agent with given input and context
    async fn execute(&self, input: &str, context: &AgentContext) -> Result<String>;

    /// Get the agent's system prompt
    fn system_prompt(&self) -> String;

    /// Get the agent type
    fn agent_type(&self) -> AgentType;
}
