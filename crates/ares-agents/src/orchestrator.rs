use crate::{Agent, AgentRegistry, AgentResponse};
use ares_config::AresConfigManager;
use ares_llm::LLMClient;
use ares_types::types::{AgentContext, AgentType, AppError, Result};
use async_trait::async_trait;
use std::sync::Arc;

/// Orchestrator agent that coordinates multiple specialized agents.
///
/// This agent decomposes complex queries into subtasks and delegates
/// them to appropriate specialized agents via the AgentRegistry.
pub struct OrchestratorAgent {
    llm: Box<dyn LLMClient>,
    config_manager: Arc<AresConfigManager>,
    agent_registry: Arc<AgentRegistry>,
}

impl OrchestratorAgent {
    /// Creates a new OrchestratorAgent with the given dependencies.
    pub fn new(
        llm: Box<dyn LLMClient>,
        config_manager: Arc<AresConfigManager>,
        agent_registry: Arc<AgentRegistry>,
    ) -> Self {
        Self {
            llm,
            config_manager,
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
        let resp = agent.execute(task, context).await?;
        Ok(resp.content)
    }
}

#[async_trait]
impl Agent for OrchestratorAgent {
    async fn execute(&self, input: &str, context: &AgentContext) -> Result<AgentResponse> {
        // Decompose the task into subtasks
        let subtasks = self.decompose_task(input).await?;

        if subtasks.is_empty() {
            let content = self.llm.generate(input).await?;
            return Ok(AgentResponse { content, usage: None, metadata: None });
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

        let content = self.llm.generate(&synthesis_prompt).await?;
        Ok(AgentResponse { content, usage: None, metadata: None })
    }

    fn system_prompt(&self) -> String {
        // Get system prompt from config if available
        let config = self.config_manager.config();
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

#[cfg(test)]
mod tests {
    use super::*;
    use ares_config::toml_config::{
        AgentConfig, AuthConfig, BillingConfig, DatabaseConfig, DynamicConfigPaths, ModelConfig,
        ProviderConfig, RagConfig, ServerConfig, AresConfig,
    };
    use ares_llm::{LLMClient, LLMResponse, ProviderRegistry};
    use ares_tools::registry::ToolRegistry;
    use ares_types::types::ToolDefinition;
    use async_trait::async_trait;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct ScriptedLlm {
        responses: Arc<Mutex<VecDeque<String>>>,
        system_prompts: Arc<Mutex<Vec<String>>>,
    }

    impl ScriptedLlm {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into_iter().map(str::to_string).collect())),
                system_prompts: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn system_prompts(&self) -> Vec<String> {
            self.system_prompts.lock().unwrap().clone()
        }

        fn next_response(&self) -> String {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| "fallback-response".to_string())
        }
    }

    #[async_trait]
    impl LLMClient for ScriptedLlm {
        fn model_name(&self) -> &str {
            "scripted-test"
        }
        async fn generate(&self, _: &str) -> Result<String> {
            Ok(self.next_response())
        }
        async fn generate_with_system(&self, system: &str, _: &str) -> Result<String> {
            self.system_prompts.lock().unwrap().push(system.to_string());
            Ok(self.next_response())
        }
        async fn generate_with_history(&self, _: &[(String, String)]) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: self.next_response(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                usage: None,
            })
        }
        async fn generate_with_tools(&self, _: &str, _: &[ToolDefinition]) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: self.next_response(),
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
                content: self.next_response(),
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

    fn sample_agent_config() -> AgentConfig {
        AgentConfig {
            model: "default".to_string(),
            system_prompt: None,
            tools: vec![],
            max_tool_iterations: 10,
            parallel_tools: false,
            extra: HashMap::new(),
        }
    }

    fn create_test_ares_config(agents: HashMap<String, AgentConfig>) -> AresConfig {
        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            database: DatabaseConfig::default(),
            providers: HashMap::new(),
            models: HashMap::new(),
            tools: HashMap::new(),
            agents,
            workflows: HashMap::new(),
            rag: RagConfig::default(),
            billing: BillingConfig::default(),
            config: DynamicConfigPaths::default(),
        }
    }

    fn create_test_provider_registry() -> Arc<ProviderRegistry> {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "ollama-local",
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".to_string(),
                default_model: "ministral-3:3b".to_string(),
            },
        );
        registry.register_model(
            "default",
            ModelConfig {
                provider: "ollama-local".to_string(),
                model: "ministral-3:3b".to_string(),
                temperature: 0.7,
                max_tokens: 512,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
            },
        );
        Arc::new(registry)
    }


    fn create_test_provider_registry_with_base_url(base_url: &str) -> Arc<ProviderRegistry> {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "ollama-local",
            ProviderConfig::Ollama {
                base_url: base_url.to_string(),
                default_model: "ministral-3:3b".to_string(),
            },
        );
        registry.register_model(
            "default",
            ModelConfig {
                provider: "ollama-local".to_string(),
                model: "ministral-3:3b".to_string(),
                temperature: 0.7,
                max_tokens: 512,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
            },
        );
        Arc::new(registry)
    }

    fn build_registry_with_provider(agent_names: &[&str], base_url: &str) -> Arc<AgentRegistry> {
        let provider_registry = create_test_provider_registry_with_base_url(base_url);
        let tool_registry = Arc::new(ToolRegistry::new());
        let mut registry = AgentRegistry::new(provider_registry, tool_registry);
        for name in agent_names {
            registry.register(name, sample_agent_config());
        }
        registry.register("orchestrator", sample_agent_config());
        registry.register("router", sample_agent_config());
        Arc::new(registry)
    }

    fn chat_done_json(content: &str) -> String {
        serde_json::json!({
            "model": "test-model",
            "created_at": "2024-01-01T00:00:00Z",
            "message": { "role": "assistant", "content": content },
            "done": true
        })
        .to_string()
    }

    fn build_registry(agent_names: &[&str]) -> Arc<AgentRegistry> {
        let provider_registry = create_test_provider_registry();
        let tool_registry = Arc::new(ToolRegistry::new());
        let mut registry = AgentRegistry::new(provider_registry, tool_registry);
        for name in agent_names {
            registry.register(name, sample_agent_config());
        }
        registry.register("orchestrator", sample_agent_config());
        registry.register("router", sample_agent_config());
        Arc::new(registry)
    }

    fn build_orchestrator(llm: ScriptedLlm, agents: &[&str], config: AresConfig) -> OrchestratorAgent {
        OrchestratorAgent::new(
            Box::new(llm),
            Arc::new(AresConfigManager::from_config(config)),
            build_registry(agents),
        )
    }

    #[tokio::test]
    async fn test_decompose_task_parses_valid_json() {
        let orch = build_orchestrator(
            ScriptedLlm::new(vec![r#"[{"agent":"sales","task":"Get Q1 revenue"},{"agent":"product","task":"Top SKUs"}]"#]),
            &["sales", "product"],
            create_test_ares_config(HashMap::new()),
        );
        let tasks = orch.decompose_task("Quarterly business review").await.expect("decompose");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0], ("sales".to_string(), "Get Q1 revenue".to_string()));
        assert_eq!(tasks[1], ("product".to_string(), "Top SKUs".to_string()));
    }

    #[tokio::test]
    async fn test_decompose_task_unknown_agent_falls_back_to_product() {
        let orch = build_orchestrator(
            ScriptedLlm::new(vec![r#"[{"agent":"unknown-agent","task":"Do something"}]"#]),
            &["product"],
            create_test_ares_config(HashMap::new()),
        );
        let tasks = orch.decompose_task("task").await.expect("decompose");
        assert_eq!(tasks, vec![("product".to_string(), "Do something".to_string())]);
    }

    #[tokio::test]
    async fn test_decompose_task_invalid_json_errors() {
        let orch = build_orchestrator(
            ScriptedLlm::new(vec!["not-json"]),
            &["product"],
            create_test_ares_config(HashMap::new()),
        );
        let err = orch.decompose_task("task").await.unwrap_err();
        assert!(matches!(err, AppError::LLM(_)));
    }

    #[tokio::test]
    async fn test_decompose_task_excludes_orchestrator_and_router_from_prompt() {
        let llm = ScriptedLlm::new(vec![r#"[]"#]);
        let llm_clone = llm.clone();
        let orch = build_orchestrator(llm, &["sales", "product"], create_test_ares_config(HashMap::new()));
        orch.decompose_task("plan").await.expect("decompose");
        let system = llm_clone.system_prompts().into_iter().next().expect("system prompt");
        assert!(system.contains("sales"));
        assert!(system.contains("product"));
        assert!(!system.contains("orchestrator"));
        assert!(!system.contains("router"));
    }

    #[tokio::test]
    async fn test_execute_with_no_subtasks_uses_direct_generation() {
        let orch = build_orchestrator(
            ScriptedLlm::new(vec![r#"[]"#, "direct-answer"]),
            &["product"],
            create_test_ares_config(HashMap::new()),
        );
        let resp = orch.execute("simple question", &test_context()).await.expect("execute");
        assert_eq!(resp.content, "direct-answer");
    }

    #[test]
    fn test_system_prompt_from_config() {
        let mut agents = HashMap::new();
        agents.insert(
            "orchestrator".to_string(),
            AgentConfig {
                model: "default".to_string(),
                system_prompt: Some("Custom orchestrator prompt".to_string()),
                tools: vec![],
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );
        let orch = build_orchestrator(ScriptedLlm::new(vec![]), &[], create_test_ares_config(agents));
        assert_eq!(orch.system_prompt(), "Custom orchestrator prompt");
    }

    #[test]
    fn test_system_prompt_default_when_missing() {
        let orch = build_orchestrator(ScriptedLlm::new(vec![]), &[], create_test_ares_config(HashMap::new()));
        assert!(orch.system_prompt().contains("orchestrator agent"));
    }

    #[test]
    fn test_agent_type_is_orchestrator() {
        let orch = build_orchestrator(ScriptedLlm::new(vec![]), &[], create_test_ares_config(HashMap::new()));
        assert_eq!(orch.agent_type(), AgentType::Orchestrator);
    }

    #[tokio::test]
    async fn test_execute_with_subtasks_delegates_and_synthesizes() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(chat_done_json("sales-agent-output")),
            )
            .mount(&server)
            .await;

        let llm = ScriptedLlm::new(vec![
            r#"[{"agent":"sales","task":"Get Q1 revenue"}]"#,
            "synthesized final answer",
        ]);
        let registry = build_registry_with_provider(&["sales"], &server.uri());
        let orch = OrchestratorAgent::new(
            Box::new(llm),
            Arc::new(AresConfigManager::from_config(create_test_ares_config(HashMap::new()))),
            registry,
        );

        let resp = orch
            .execute("quarterly business review", &test_context())
            .await
            .expect("execute with subtasks");
        assert_eq!(resp.content, "synthesized final answer");
    }

}
