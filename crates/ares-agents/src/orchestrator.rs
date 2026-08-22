use crate::{Agent, AgentRegistry, AgentResponse};
use ares_config::AresConfigManager;
use ares_llm::LLMClient;
use ares_types::types::{AgentContext, AgentType, AppError, Result};
use async_trait::async_trait;
use std::future::Future;
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

/// Join fallible subtask futures concurrently, preserving input order.
/// Returns the first error via `try_join_all`.
pub(crate) async fn join_subtask_results<T, E, Fut>(
    futs: Vec<Fut>,
) -> std::result::Result<Vec<T>, E>
where
    Fut: Future<Output = std::result::Result<T, E>>,
{
    futures::future::try_join_all(futs).await
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

        // Execute subtasks concurrently via try_join_all
        let futs = subtasks
            .into_iter()
            .map(|(agent_name, task)| async move {
                let result = self.execute_subtask(&agent_name, &task, context).await?;
                Ok::<_, AppError>(format!("[{}] {}", agent_name, result))
            })
            .collect();
        let results = join_subtask_results(futs).await?;

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
        generate_prompts: Arc<Mutex<Vec<String>>>,
    }

    impl ScriptedLlm {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into_iter().map(str::to_string).collect())),
                system_prompts: Arc::new(Mutex::new(Vec::new())),
                generate_prompts: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn system_prompts(&self) -> Vec<String> {
            self.system_prompts.lock().unwrap().clone()
        }

        fn generate_prompts(&self) -> Vec<String> {
            self.generate_prompts.lock().unwrap().clone()
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
        async fn generate(&self, prompt: &str) -> Result<String> {
            self.generate_prompts.lock().unwrap().push(prompt.to_string());
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
            allowed_tools: None,
            extra: HashMap::new(),
        }
    }

    fn create_test_ares_config(agents: HashMap<String, AgentConfig>) -> AresConfig {
        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            database: DatabaseConfig::default(),
            nvidia: None,
            providers: HashMap::new(),
            models: HashMap::new(),
            tools: HashMap::new(),
            agents,
            workflows: HashMap::new(),
            rag: RagConfig::default(),
            billing: BillingConfig::default(),
            skills: None,
            config: DynamicConfigPaths::default(),
        }
    }

    fn create_test_provider_registry() -> Arc<ProviderRegistry> {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "ollama-local",
            ProviderConfig::Ollama {
                api_key_env: "TEST_KEY".to_string(),
                base_url: "https://test.example.com".to_string(),
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
            },
        );
        Arc::new(registry)
    }


    fn create_test_provider_registry_with_base_url(base_url: &str) -> Arc<ProviderRegistry> {
        let mut registry = ProviderRegistry::new();
        registry.register_provider(
            "ollama-local",
            ProviderConfig::Ollama {
                api_key_env: "TEST_KEY".to_string(),
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

    #[tokio::test]
    async fn test_execute_subtasks_run_concurrently() {
        use std::time::{Duration, Instant};

        async fn sleep_ok(label: &'static str) -> std::result::Result<&'static str, &'static str> {
            tokio::time::sleep(Duration::from_millis(80)).await;
            Ok(label)
        }

        let start = Instant::now();
        let results = join_subtask_results(vec![sleep_ok("a"), sleep_ok("b")])
            .await
            .expect("join");
        let elapsed = start.elapsed();
        assert_eq!(results, vec!["a", "b"]);
        assert!(
            elapsed < Duration::from_millis(140),
            "expected concurrent join under 140ms, got {elapsed:?}"
        );
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
            allowed_tools: None,
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

    #[tokio::test]
    async fn test_decompose_task_defaults_missing_json_fields() {
        let orch = build_orchestrator(
            ScriptedLlm::new(vec![r#"[{"task":"task-only"},{"agent":"sales"}]"#]),
            &["sales", "product"],
            create_test_ares_config(HashMap::new()),
        );
        let tasks = orch.decompose_task("plan").await.expect("decompose");
        assert_eq!(
            tasks,
            vec![
                ("product".to_string(), "task-only".to_string()),
                ("sales".to_string(), String::new()),
            ]
        );
    }

    #[tokio::test]
    async fn test_decompose_task_mixed_known_and_unknown_agents() {
        let orch = build_orchestrator(
            ScriptedLlm::new(vec![
                r#"[{"agent":"sales","task":"Revenue"},{"agent":"ghost","task":"Haunt"},{"agent":"finance","task":"Budget"}]"#,
            ]),
            &["sales", "finance", "product"],
            create_test_ares_config(HashMap::new()),
        );
        let tasks = orch.decompose_task("mixed").await.expect("decompose");
        assert_eq!(
            tasks,
            vec![
                ("sales".to_string(), "Revenue".to_string()),
                ("product".to_string(), "Haunt".to_string()),
                ("finance".to_string(), "Budget".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn test_decompose_task_only_orchestrator_router_yields_empty_agent_list() {
        let llm = ScriptedLlm::new(vec![r#"[]"#]);
        let llm_clone = llm.clone();
        let orch = build_orchestrator(llm, &[], create_test_ares_config(HashMap::new()));
        orch.decompose_task("plan").await.expect("decompose");
        let system = llm_clone.system_prompts().into_iter().next().expect("system prompt");
        assert!(system.contains("Available agents: "));
        assert!(!system.contains("orchestrator"));
        assert!(!system.contains("router"));
    }

    #[tokio::test]
    async fn test_execute_subtask_returns_registered_agent_content() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_done_json("subtask-body")))
            .mount(&server)
            .await;

        let orch = OrchestratorAgent::new(
            Box::new(ScriptedLlm::new(vec![])),
            Arc::new(AresConfigManager::from_config(create_test_ares_config(HashMap::new()))),
            build_registry_with_provider(&["sales"], &server.uri()),
        );
        let content = orch
            .execute_subtask("sales", "Get Q1 revenue", &test_context())
            .await
            .expect("subtask");
        assert_eq!(content, "subtask-body");
    }

    #[tokio::test]
    async fn test_execute_subtask_unknown_agent_errors() {
        let orch = build_orchestrator(
            ScriptedLlm::new(vec![]),
            &["product"],
            create_test_ares_config(HashMap::new()),
        );
        let err = orch
            .execute_subtask("missing-agent", "task", &test_context())
            .await
            .unwrap_err();
        assert!(!matches!(err, AppError::LLM(_)));
    }

    #[tokio::test]
    async fn test_execute_multiple_subtasks_formats_results_for_synthesis() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(chat_done_json("agent-output")))
            .mount(&server)
            .await;

        let llm = ScriptedLlm::new(vec![
            r#"[{"agent":"sales","task":"Revenue"},{"agent":"finance","task":"Budget"}]"#,
            "combined answer",
        ]);
        let llm_clone = llm.clone();
        let orch = OrchestratorAgent::new(
            Box::new(llm),
            Arc::new(AresConfigManager::from_config(create_test_ares_config(HashMap::new()))),
            build_registry_with_provider(&["sales", "finance"], &server.uri()),
        );

        let resp = orch
            .execute("annual review", &test_context())
            .await
            .expect("execute");
        assert_eq!(resp.content, "combined answer");
        let synthesis = llm_clone.generate_prompts().into_iter().next().expect("synthesis prompt");
        assert!(synthesis.contains("Original query: annual review"));
        assert!(synthesis.contains("[sales] agent-output"));
        assert!(synthesis.contains("[finance] agent-output"));
        assert!(synthesis.contains("Subtask results:"));
    }

    #[test]
    fn test_agent_type_orchestrator_serde_roundtrip() {
        let agent = AgentType::Orchestrator;
        let json = serde_json::to_string(&agent).expect("serialize");
        assert_eq!(json, "\"orchestrator\"");
        let parsed: AgentType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, agent);
        assert_eq!(format!("{parsed:?}"), "Orchestrator");
        assert_eq!(agent.clone(), AgentType::Orchestrator);
    }

    #[test]
    fn test_agent_context_debug_clone() {
        let ctx = test_context();
        let cloned = ctx.clone();
        assert_eq!(cloned.user_id, "test-user");
        assert_eq!(cloned.session_id, "test-session");
        assert!(format!("{ctx:?}").contains("AgentContext"));
    }

    #[test]
    fn test_agent_response_none_usage_and_metadata() {
        let resp = AgentResponse {
            content: "ok".to_string(),
            usage: None,
            metadata: None,
        };
        assert_eq!(resp.content, "ok");
        assert!(resp.usage.is_none());
        assert!(resp.metadata.is_none());
    }


}
