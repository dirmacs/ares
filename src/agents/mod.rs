pub mod configurable;
pub mod orchestrator;
pub mod registry;
pub mod router;

// Legacy agents - deprecated in 0.2.0, kept for backward compatibility
// These will be removed in a future version. Use ConfigurableAgent instead.
#[deprecated(
    since = "0.2.0",
    note = "Legacy agents are deprecated. Use ConfigurableAgent with TOML configuration instead."
)]
pub mod finance;
#[deprecated(
    since = "0.2.0",
    note = "Legacy agents are deprecated. Use ConfigurableAgent with TOML configuration instead."
)]
pub mod hr;
#[deprecated(
    since = "0.2.0",
    note = "Legacy agents are deprecated. Use ConfigurableAgent with TOML configuration instead."
)]
pub mod invoice;
#[deprecated(
    since = "0.2.0",
    note = "Legacy agents are deprecated. Use ConfigurableAgent with TOML configuration instead."
)]
pub mod product;
#[deprecated(
    since = "0.2.0",
    note = "Legacy agents are deprecated. Use ConfigurableAgent with TOML configuration instead."
)]
pub mod sales;

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
