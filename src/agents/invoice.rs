//! Legacy Invoice Agent
//!
//! **DEPRECATED**: This module is deprecated since v0.2.0.
//! Use `ConfigurableAgent` with `ares.toml` configuration instead.
//!
//! # Migration
//!
//! Configure agents in `ares.toml`:
//!
//! ```toml
//! [agents.invoice]
//! model = "balanced"
//! tools = ["calculator"]
//! system_prompt = "You are an Invoice Agent..."
//! ```
//!
//! Then use `AgentRegistry::create_agent("invoice")` to create the agent.

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
pub struct InvoiceAgent {
    llm: Box<dyn LLMClient>,
}

#[allow(deprecated)]
impl InvoiceAgent {
    pub fn new(llm: Box<dyn LLMClient>) -> Self {
        Self { llm }
    }
}

#[allow(deprecated)]
#[async_trait]
impl Agent for InvoiceAgent {
    async fn execute(&self, input: &str, _context: &AgentContext) -> Result<String> {
        self.llm
            .generate_with_system(&self.system_prompt(), input)
            .await
    }

    fn system_prompt(&self) -> String {
        "You are an Invoice Agent specialized in invoice processing, billing, and payment queries."
            .to_string()
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Invoice
    }
}
