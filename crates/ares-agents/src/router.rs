use crate::{Agent, AgentResponse};
use ares_llm::LLMClient;
use ares_types::types::{AgentContext, AgentType, Result};
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
    async fn execute(&self, _input: &str, _context: &AgentContext) -> Result<AgentResponse> {
        // Note: RouterAgent.route() is called by the orchestrator/chat handler,
        // not through the Agent trait execute() method. This is a placeholder.
        Ok(AgentResponse {
            content: "router".to_string(),
            usage: None,
            metadata: None,
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use ares_llm::{LLMClient, LLMResponse};
    use ares_types::types::ToolDefinition;
    use async_trait::async_trait;

    struct RoutingLlm {
        label: String,
    }

    impl RoutingLlm {
        fn new(label: impl Into<String>) -> Self {
            Self { label: label.into() }
        }
    }

    #[async_trait]
    impl LLMClient for RoutingLlm {
        fn model_name(&self) -> &str {
            "routing-test"
        }
        async fn generate(&self, _: &str) -> Result<String> {
            Ok(self.label.clone())
        }
        async fn generate_with_system(&self, _: &str, _: &str) -> Result<String> {
            Ok(self.label.clone())
        }
        async fn generate_with_history(&self, _: &[(String, String)]) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: self.label.clone(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: None,
            })
        }
        async fn generate_with_tools(&self, _: &str, _: &[ToolDefinition]) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: self.label.clone(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: None,
            })
        }
        async fn generate_with_tools_and_history(
            &self,
            _: &[ares_llm::coordinator::ConversationMessage],
            _: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: self.label.clone(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: None,
            })
        }
        async fn stream(&self, _: &str) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }
        async fn stream_with_system(&self, _: &str, _: &str) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }
        async fn stream_with_history(&self, _: &[(String, String)]) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Ok(Box::new(futures::stream::empty()))
        }
    }

    fn test_context() -> AgentContext {
        AgentContext {
            user_id: "test-user".to_string(),
            session_id: "test-session".to_string(),
            conversation_history: vec![],
            user_memory: None,
        }
    }

    #[test]
    fn test_parse_routing_decision_exact_match() {
        assert_eq!(RouterAgent::parse_routing_decision("product"), Some("product".to_string()));
        assert_eq!(RouterAgent::parse_routing_decision("  SALES  "), Some("sales".to_string()));
    }

    #[test]
    fn test_parse_routing_decision_embedded_and_suffix() {
        assert_eq!(
            RouterAgent::parse_routing_decision("I would route this to product"),
            Some("product".to_string())
        );
        assert_eq!(RouterAgent::parse_routing_decision("product agent"), Some("product".to_string()));
        assert_eq!(RouterAgent::parse_routing_decision("Route: finance."), Some("finance".to_string()));
    }

    #[test]
    fn test_parse_routing_decision_unknown() {
        assert_eq!(RouterAgent::parse_routing_decision("unknown-bot"), None);
        assert_eq!(RouterAgent::parse_routing_decision(""), None);
    }

    #[test]
    fn test_parse_routing_decision_substring_containment() {
        assert_eq!(
            RouterAgent::parse_routing_decision("our-products-catalog"),
            Some("product".to_string())
        );
        assert_eq!(
            RouterAgent::parse_routing_decision("enterprise-salesforce-data"),
            Some("sales".to_string())
        );
    }

    #[tokio::test]
    async fn test_route_maps_specialized_agents() {
        let ctx = test_context();
        let cases = [
            ("product", AgentType::Product),
            ("invoice", AgentType::Invoice),
            ("sales", AgentType::Sales),
            ("finance", AgentType::Finance),
            ("hr", AgentType::HR),
        ];
        for (label, expected) in cases {
            let router = RouterAgent::new(Box::new(RoutingLlm::new(label)));
            assert_eq!(router.route("query", &ctx).await.expect("route"), expected);
        }
    }

    #[tokio::test]
    async fn test_route_research_and_orchestrator_labels() {
        let ctx = test_context();
        for label in ["orchestrator", "research"] {
            let router = RouterAgent::new(Box::new(RoutingLlm::new(label)));
            assert_eq!(router.route("complex query", &ctx).await.expect("route"), AgentType::Orchestrator);
        }
    }

    #[tokio::test]
    async fn test_route_defaults_to_orchestrator_on_unparseable_output() {
        let router = RouterAgent::new(Box::new(RoutingLlm::new("definitely-not-an-agent")));
        assert_eq!(
            router.route("anything", &test_context()).await.expect("route"),
            AgentType::Orchestrator
        );
    }

    #[test]
    fn test_system_prompt_lists_available_agents() {
        let prompt = RouterAgent::new(Box::new(RoutingLlm::new("product"))).system_prompt();
        for agent in VALID_AGENTS {
            if *agent == "research" {
                continue;
            }
            assert!(prompt.contains(agent), "expected routing prompt to mention {agent}");
        }
        assert!(prompt.contains("orchestrator"));
    }

    #[test]
    fn test_agent_type_is_router() {
        assert_eq!(RouterAgent::new(Box::new(RoutingLlm::new("product"))).agent_type(), AgentType::Router);
    }

    #[tokio::test]
    async fn test_execute_placeholder_response() {
        let router = RouterAgent::new(Box::new(RoutingLlm::new("product")));
        let resp = router.execute("ignored", &test_context()).await.expect("execute");
        assert_eq!(resp.content, "router");
    }
}
