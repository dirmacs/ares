//! Legacy Finance Agent
//!
//! **DEPRECATED**: This module is deprecated since v0.2.0.
//! Use `ConfigurableAgent` with `ares.toml` configuration instead.
//!
//! # Migration
//!
//! Configure agents in `ares.toml`:
//!
//! ```toml
//! [agents.finance]
//! model = "balanced"
//! tools = ["calculator"]
//! system_prompt = "You are a Finance Agent..."
//! ```
//!
//! Then use `AgentRegistry::create_agent("finance")` to create the agent.

use crate::{
    agents::Agent,
    llm::LLMClient,
    types::{AgentContext, AgentType, Result},
};
use async_trait::async_trait;

#[deprecated(
    since = "0.2.0",
    note = "Use ConfigurableAgent with ares.toml configuration instead. See agents/configurable.rs"
)]
pub struct FinanceAgent {
    llm: Box<dyn LLMClient>,
}

#[allow(deprecated)]
impl FinanceAgent {
    pub fn new(llm: Box<dyn LLMClient>) -> Self {
        Self { llm }
    }
}

#[allow(deprecated)]
#[async_trait]
impl Agent for FinanceAgent {
    async fn execute(&self, input: &str, _context: &AgentContext) -> Result<String> {
        self.llm
            .generate_with_system(&self.system_prompt(), input)
            .await
    }

    fn system_prompt(&self) -> String {
        "You are a Finance Agent specialized in financial analysis, budgeting, and expense management.".to_string()
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Finance
    }
}
