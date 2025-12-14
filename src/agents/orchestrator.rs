use crate::{
    AppState,
    agents::{Agent, finance, hr, invoice, product, sales},
    llm::LLMClient,
    types::{AgentContext, AgentType, AppError, Result},
};
use async_trait::async_trait;

pub struct OrchestratorAgent {
    llm: Box<dyn LLMClient>,
    state: AppState,
}

impl OrchestratorAgent {
    pub fn new(llm: Box<dyn LLMClient>, state: AppState) -> Self {
        Self { llm, state }
    }

    async fn decompose_task(&self, input: &str) -> Result<Vec<(AgentType, String)>> {
        let system_prompt = r#"You are a task decomposition agent. Break down complex queries into subtasks for specialized agents.

Available agents: product, invoice, sales, finance, hr

Return a JSON array of tasks:
[
    {"agent": "sales", "task": "Get Q1 revenue"},
    {"agent": "product", "task": "List top products"}
]

Only respond with valid JSON."#;

        let response = self.llm.generate_with_system(system_prompt, input).await?;

        // Parse JSON response
        let tasks: Vec<serde_json::Value> = serde_json::from_str(&response)
            .map_err(|e| AppError::LLM(format!("Failed to parse tasks: {}", e)))?;

        let mut result = Vec::new();
        for task in tasks {
            let agent_str = task["agent"].as_str().unwrap_or("product");
            let task_str = task["task"].as_str().unwrap_or("");

            let agent_type = match agent_str {
                "product" => AgentType::Product,
                "invoice" => AgentType::Invoice,
                "sales" => AgentType::Sales,
                "finance" => AgentType::Finance,
                "hr" => AgentType::HR,
                _ => AgentType::Product,
            };

            result.push((agent_type, task_str.to_string()));
        }

        Ok(result)
    }

    async fn execute_subtask(
        &self,
        agent_type: AgentType,
        task: &str,
        context: &AgentContext,
    ) -> Result<String> {
        // Get agent-specific model from config
        let config = self.state.config_manager.config();
        let agent_name = match agent_type {
            AgentType::Product => "product",
            AgentType::Invoice => "invoice",
            AgentType::Sales => "sales",
            AgentType::Finance => "finance",
            AgentType::HR => "hr",
            _ => "orchestrator",
        };

        let model_name = config
            .get_agent(agent_name)
            .map(|a| a.model.as_str())
            .unwrap_or("balanced");

        let llm = match self
            .state
            .provider_registry
            .create_client_for_model(model_name)
            .await
        {
            Ok(client) => client,
            Err(_) => self.state.llm_factory.create_default().await?,
        };

        match agent_type {
            AgentType::Product => {
                let agent = product::ProductAgent::new(llm);
                agent.execute(task, context).await
            }
            AgentType::Invoice => {
                let agent = invoice::InvoiceAgent::new(llm);
                agent.execute(task, context).await
            }
            AgentType::Sales => {
                let agent = sales::SalesAgent::new(llm);
                agent.execute(task, context).await
            }
            AgentType::Finance => {
                let agent = finance::FinanceAgent::new(llm);
                agent.execute(task, context).await
            }
            AgentType::HR => {
                let agent = hr::HrAgent::new(llm);
                agent.execute(task, context).await
            }
            _ => Err(AppError::InvalidInput("Invalid agent type".to_string())),
        }
    }
}

#[async_trait]
impl Agent for OrchestratorAgent {
    async fn execute(&self, input: &str, context: &AgentContext) -> Result<String> {
        // Decompose the task
        let subtasks = self.decompose_task(input).await?;

        if subtasks.is_empty() {
            return self.llm.generate(input).await;
        }

        // Execute subtasks in parallel
        let mut results = Vec::new();
        for (agent_type, task) in subtasks {
            let result = self.execute_subtask(agent_type, &task, context).await?;
            results.push(format!("{:?} Result: {}", agent_type, result));
        }

        // Synthesize results
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
