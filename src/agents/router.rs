use crate::{
    agents::Agent,
    llm::LLMClient,
    types::{AgentContext, AgentType, Result},
};
use async_trait::async_trait;

/// Valid agent names for routing
const VALID_AGENTS: &[&str] = &[
    "product",
    "invoice",
    "sales",
    "finance",
    "hr",
    "orchestrator",
    "research",
];

/// Router agent that directs queries to specialized agents.
///
/// Uses an LLM to analyze user queries and determine which
/// specialized agent is best suited to handle them.
pub struct RouterAgent {
    llm: Box<dyn LLMClient>,
}

impl RouterAgent {
    /// Creates a new RouterAgent with the given LLM client.
    pub fn new(llm: Box<dyn LLMClient>) -> Self {
        Self { llm }
    }

    /// Parse routing decision from LLM output
    ///
    /// This handles various LLM output formats:
    /// - Clean output: "product"
    /// - With whitespace: "  product  "
    /// - With extra text: "I would route this to product"
    /// - Agent suffix: "product agent"
    fn parse_routing_decision(output: &str) -> Option<String> {
        let trimmed = output.trim().to_lowercase();

        // First, try exact match
        if VALID_AGENTS.contains(&trimmed.as_str()) {
            return Some(trimmed);
        }

        // Try to extract valid agent name from output
        // Split by common delimiters and check each word
        for word in trimmed.split(|c: char| c.is_whitespace() || c == ':' || c == ',' || c == '.') {
            let word = word.trim();
            if VALID_AGENTS.contains(&word) {
                return Some(word.to_string());
            }
        }

        // Check if any valid agent name is contained in the output
        for agent in VALID_AGENTS {
            if trimmed.contains(agent) {
                return Some(agent.to_string());
            }
        }

        None
    }

    /// Routes a query to the appropriate agent type.
    pub async fn route(&self, query: &str, _context: &AgentContext) -> Result<AgentType> {
        let system_prompt = self.system_prompt();
        let response = self.llm.generate_with_system(&system_prompt, query).await?;

        // Parse the response with robust matching
        let agent_name = Self::parse_routing_decision(&response);

        match agent_name.as_deref() {
            Some("product") => Ok(AgentType::Product),
            Some("invoice") => Ok(AgentType::Invoice),
            Some("sales") => Ok(AgentType::Sales),
            Some("finance") => Ok(AgentType::Finance),
            Some("hr") => Ok(AgentType::HR),
            Some("orchestrator") | Some("research") => Ok(AgentType::Orchestrator),
            _ => {
                // Default to orchestrator for complex queries or unrecognized routing
                tracing::debug!(
                    "Router could not parse output '{}', defaulting to orchestrator",
                    response
                );
                Ok(AgentType::Orchestrator)
            }
        }
    }
}

#[async_trait]
impl Agent for RouterAgent {
    async fn execute(&self, _input: &str, _context: &AgentContext) -> Result<String> {
        // Note: RouterAgent.route() is called by the orchestrator/chat handler,
        // not through the Agent trait execute() method. This is a placeholder.
        // If called directly, return the router agent name.
        Ok("router".to_string())
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
