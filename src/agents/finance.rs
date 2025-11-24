use crate::{
    agents::Agent,
    llm::LLMClient,
    types::{AgentContext, AgentType, Result},
};
use async_trait::async_trait;

pub struct FinanceAgent {
    llm: Box<dyn LLMClient>,
}

impl FinanceAgent {
    pub fn new(llm: Box<dyn LLMClient>) -> Self {
        Self { llm }
    }
}

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
