use crate::{
    agents::Agent,
    llm::LLMClient,
    types::{AgentContext, AgentType, AppError, Result},
};
use async_trait::async_trait;

pub struct RouterAgent {
    llm: Box<dyn LLMClient>,
}

impl RouterAgent {
    pub fn new(llm: Box<dyn LLMClient>) -> Self {
        Self { llm }
    }

    // Route the query to the appropriate agent
    pub async fn route(&self, query: &str, _context: &AgentContext) -> Result<AgentType> {
        let system_prompt = self.system_prompt();
        let response = self.llm.generate_with_system(&system_prompt, query).await?;

        // Parse the response to determine agent type

        let agent_type = response.trim().to_lowercase();

        match agent_type.as_str() {
            "product" => Ok(AgentType::Product),
            "invoice" => Ok(AgentType::Invoice),
            "sales" => Ok(AgentType::Sales),
            "finance" => Ok(AgentType::Finance),
            "hr" => Ok(AgentType::HR),
            "orchestrator" => Ok(AgentType::Orchestrator),
            _ => {
                // Default to orchestrator for complex queries
                Ok(AgentType::Orchestrator)
            }
        }
    }
}

#[async_trait]
impl Agent for RouterAgent {
    async fn execute(&self, input: &str, context: &AgentContext) -> Result<String> {
        let agent_type = self.route(input, context).await?;
        Ok(format!("{:?}", agent_type))
    }

    fn system_prompt(&self) -> String {
        r#"You are a routing agent that classifies user queries and routes them to the appropriate specialized agent.

Available agents:
- product: Product information, recommendations, catalog queries
- invoice: Invoice processing, billing questions, payment status
- sales: Sales data, analytics, performance metrics
- finance: Financial reports, budgets, expense analysis
- hr: Human resources, employee information, policies
- orchestrator: Complex queries requiring multiple agents or research

Analyze the user's query and respond with ONLY the agent name (lowercase, one word).
Examples:
- "What products do we have?" → product
- "Show me last quarter's sales" → sales
- "What's our hiring policy?" → hr
- "Create a comprehensive market analysis" → orchestrator

Respond with ONLY the agent name, nothing else."#.to_string()
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Router
    }
}
