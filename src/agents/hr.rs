use crate::{
    agents::Agent,
    llm::LLMCLient,
    types::{AgentContext, AgentType, Result},
};
use async_trait::async_trait;

pub struct HrAgent {
    llm: Box<dyn LLMClient>,
}

impl HrAgent {
    pub fn new(llm: Box<dyn LLMClient>) -> Self {
        Self { llm }
    }
}

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
