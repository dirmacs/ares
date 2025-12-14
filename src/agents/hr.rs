//! Legacy HR Agent
//!
//! **DEPRECATED**: This module is deprecated since v0.2.0.
//! Use `ConfigurableAgent` with `ares.toml` configuration instead.
//!
//! # Migration
//!
//! Configure agents in `ares.toml`:
//!
//! ```toml
//! [agents.hr]
//! model = "balanced"
//! tools = []
//! system_prompt = "You are an HR Agent..."
//! ```
//!
//! Then use `AgentRegistry::create_agent("hr")` to create the agent.

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
pub struct HrAgent {
    llm: Box<dyn LLMClient>,
}

#[allow(deprecated)]
impl HrAgent {
    pub fn new(llm: Box<dyn LLMClient>) -> Self {
        Self { llm }
    }
}

#[allow(deprecated)]
#[async_trait]
impl Agent for HrAgent {
    async fn execute(&self, input: &str, _context: &AgentContext) -> Result<String> {
        self.llm
            .generate_with_system(&self.system_prompt(), input)
            .await
    }

    fn system_prompt(&self) -> String {
        "You are an HR Agent specialized in human resources, employee management, and policies."
            .to_string()
    }

    fn agent_type(&self) -> AgentType {
        AgentType::HR
    }
}
