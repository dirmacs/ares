use crate::{
    agents::Agent,
    llm::LLMClient,
    types::{AgentContext, AgentType, Result},
};
use async_trait::async_trait;

pub struct SalesAgent {
    llm: Box<dyn LLMClient>,
}

impl SalesAgent {
    pub fn new(llm: Box<dyn LLMClient>) -> Self {
        Self { llm }
    }
}

#[async_trait]
impl Agent for SalesAgent {
    async fn execute(&self, input: &str, _context: &AgentContext) -> Result<String> {
        self.llm
            .generate_with_system(&self.system_prompt(), input)
            .await
    }

    fn system_prompt(&self) -> String {
        "You are a Sales Agent specialized in sales analytics, performance metrics, and revenue "
            .to_string()
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Sales
    }
}
