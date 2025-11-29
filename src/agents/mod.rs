pub mod finance;
pub mod hr;
pub mod invoice;
pub mod orchestrator;
pub mod product;
pub mod router;
pub mod sales;

use crate::types::{AgentContext, AgentType, Result};
use async_trait::async_trait;

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
