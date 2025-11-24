use crate::{
    agents::Agent,
    llm::LLMClient,
    types::{AgentContext, AgentType, Result},
};
use async_trait::async_trait;

pub struct InvoiceAgent {
    llm: Box<dyn LLMClient>,
}

impl InvoiceAgent {
    pub fn new(llm: Box<dyn LLMClient>) -> Self {
        Self { llm }
    }
}

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
