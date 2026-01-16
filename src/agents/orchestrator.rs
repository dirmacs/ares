use crate::{
    agents::{Agent, AgentRegistry},
    llm::LLMClient,
    types::{AgentContext, AgentType, AppError, Result},
    AppState,
};
use async_trait::async_trait;
use std::sync::Arc;

/// Orchestrator agent that coordinates multiple specialized agents.
///
/// This agent decomposes complex queries into subtasks and delegates
/// them to appropriate specialized agents via the AgentRegistry.
pub struct OrchestratorAgent {
    llm: Box<dyn LLMClient>,
    state: AppState,
    agent_registry: Arc<AgentRegistry>,
}

impl OrchestratorAgent {
    /// Creates a new OrchestratorAgent with the given dependencies.
    pub fn new(
        llm: Box<dyn LLMClient>,
        state: AppState,
        agent_registry: Arc<AgentRegistry>,
    ) -> Self {
        Self {
            llm,
            state,
            agent_registry,
        }
    }

    /// Decompose a complex task into subtasks for specialized agents
    async fn decompose_task(&self, input: &str) -> Result<Vec<(String, String)>> {
        // Get available agents from registry
        let available_agents = self.agent_registry.agent_names();
        let agent_list = available_agents
            .iter()
            .filter(|name| **name != "orchestrator" && **name != "router")
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");

        let system_prompt = format!(
            r#"You are a task decomposition agent. Break down complex queries into subtasks for specialized agents.

Available agents: {}

Return a JSON array of tasks:
[
    {{"agent": "sales", "task": "Get Q1 revenue"}},
    {{"agent": "product", "task": "List top products"}}
]

Only respond with valid JSON."#,
            agent_list
        );

        let response = self.llm.generate_with_system(&system_prompt, input).await?;

        // Parse JSON response
        let tasks: Vec<serde_json::Value> = serde_json::from_str(&response)
            .map_err(|e| AppError::LLM(format!("Failed to parse tasks: {}", e)))?;

        let mut result = Vec::new();
        for task in tasks {
            let agent_name = task["agent"].as_str().unwrap_or("product").to_string();
            let task_str = task["task"].as_str().unwrap_or("").to_string();

            // Validate agent exists in registry
            if self.agent_registry.has_agent(&agent_name) {
                result.push((agent_name, task_str));
            } else {
                // Fall back to product agent if unknown
                result.push(("product".to_string(), task_str));
            }
        }

        Ok(result)
    }

    /// Execute a subtask using the appropriate agent from the registry
    async fn execute_subtask(
        &self,
        agent_name: &str,
        task: &str,
        context: &AgentContext,
    ) -> Result<String> {
        // Create agent from registry (handles model and tool configuration)
        let agent = self.agent_registry.create_agent(agent_name).await?;
        agent.execute(task, context).await
    }
}

#[async_trait]
impl Agent for OrchestratorAgent {
    async fn execute(&self, input: &str, context: &AgentContext) -> Result<String> {
        // Decompose the task into subtasks
        let subtasks = self.decompose_task(input).await?;

        if subtasks.is_empty() {
            return self.llm.generate(input).await;
        }

        // Execute subtasks sequentially (could be parallelized in future)
        let mut results = Vec::new();
        for (agent_name, task) in subtasks {
            let result = self.execute_subtask(&agent_name, &task, context).await?;
            results.push(format!("[{}] {}", agent_name, result));
        }

        // Synthesize results into final response
        let synthesis_prompt = format!(
            "Original query: {}\n\nSubtask results:\n{}\n\nProvide a comprehensive answer:",
            input,
            results.join("\n\n")
        );

        self.llm.generate(&synthesis_prompt).await
    }

    fn system_prompt(&self) -> String {
        // Get system prompt from config if available
        let config = self.state.config_manager.config();
        config
            .get_agent("orchestrator")
            .and_then(|a| a.system_prompt.clone())
            .unwrap_or_else(|| {
                "You are an orchestrator agent that coordinates multiple specialized agents to answer complex queries.".to_string()
            })
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Orchestrator
    }
}
