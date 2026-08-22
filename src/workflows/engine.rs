//! Workflow Engine
//!
//! Executes declarative workflows by orchestrating agent execution based on
//! TOML configuration.

use crate::agents::Agent;
use crate::api::handlers::user_agents::resolve_agent;
use crate::types::{AgentContext, AgentType, AppError, Result};
use crate::utils::toml_config::{AgentConfig, WorkflowConfig};
use ares_cordis_core::{Context, Service};
use crate::AppState;
use std::sync::Arc;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Output from a workflow execution
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowOutput {
    /// The final response from the workflow
    pub final_response: String,
    /// Number of steps executed
    pub steps_executed: usize,
    /// List of agent names that were used
    pub agents_used: Vec<String>,
    /// Detailed reasoning path showing each step
    pub reasoning_path: Vec<WorkflowStep>,
}

/// A single step in the workflow execution
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowStep {
    /// The agent that executed this step
    pub agent_name: String,
    /// The input provided to the agent
    pub input: String,
    /// The output from the agent
    pub output: String,
    /// Unix timestamp when this step was executed
    pub timestamp: i64,
    /// Duration of this step in milliseconds
    pub duration_ms: u64,
}

/// Valid agent names for routing
const VALID_AGENTS: &[&str] = &[
    "product",
    "invoice",
    "sales",
    "finance",
    "hr",
    "orchestrator",
    "research",
    "router",
];

/// Workflow engine that orchestrates agent execution
///
/// Cordis service — owns router delegation via `AgentResolverService` +
/// `AgentExecutionService` injected via `ctx.get`. Migrated from `AppState` to
/// `Arc<Context>` per Phase 4 step 16.
pub struct WorkflowEngine {
    /// Cordis context for resolving agents (replaces `AppState` god-struct)
    pub ctx: Arc<Context>,
}

impl Service for WorkflowEngine {
    fn name(&self) -> &'static str {
        "workflow"
    }
    fn check(&self) -> bool {
        true
    }
}

impl WorkflowEngine {
    /// Create a new workflow engine from a Cordis context.
    pub fn new(ctx: Arc<Context>) -> Self {
        Self { ctx }
    }

    /// Legacy alias for `new` — keeps `WorkflowEngine::new(state)` call-sites working
    /// where `AppState = Arc<Context>`.
    #[allow(dead_code)]
    pub fn from_ctx(ctx: Arc<Context>) -> Self {
        Self::new(ctx)
    }

    /// Parse routing decision from router output
    ///
    /// This handles various output formats:
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

    /// Execute a workflow by name
    ///
    /// # Arguments
    ///
    /// * `workflow_name` - The name of the workflow to execute (e.g., "default", "research")
    /// * `user_input` - The user's query or input
    /// * `context` - The agent context with user info and conversation history
    ///
    /// # Returns
    ///
    /// A `WorkflowOutput` containing the final response and execution details.
    pub async fn execute_workflow(
        &self,
        workflow_name: &str,
        user_input: &str,
        context: &AgentContext,
    ) -> Result<WorkflowOutput> {
        // Prove AgentResolverService + AgentExecutionService injection via ctx.get (provider-agnostic)
        let _resolver = self.ctx.get::<ares_agents::resolver::AgentResolverService>();
        let _execution = self.ctx.get::<ares_agents::execution::AgentExecutionService>();
        // Get workflow configuration via Context (migrated from AppState)
        let config = self.ctx.get::<crate::AresConfigManager>().expect("not provided").config();
        let workflow = config.get_workflow(workflow_name).ok_or_else(|| {
            AppError::Configuration(format!(
                "Workflow '{}' not found in configuration",
                workflow_name
            ))
        })?;

        let mut steps = Vec::new();
        let mut agents_used = Vec::new();
        let current_input = user_input.to_string();
        let mut current_agent_name = workflow.entry_agent.clone();
        let mut depth = 0;

        // Execute workflow with depth limiting
        while depth < workflow.max_depth {
            let step_start = std::time::Instant::now();
            let timestamp = Utc::now().timestamp();

            // Resolve agent using the 3-tier hierarchy via Context (AgentResolverService precedence)
            let (user_agent, _source) = match resolve_agent(
                &self.ctx,
                &context.user_id,
                current_agent_name.clone(),
            )
            .await
            {
                Ok(res) => res,
                Err(e) => {
                    // Try fallback agent if available
                    if let Some(fallback) = &workflow.fallback_agent {
                        tracing::warn!(
                            "Failed to resolve agent '{}', using fallback '{}'",
                            current_agent_name,
                            fallback
                        );
                        current_agent_name = fallback.clone();
                        resolve_agent(&self.ctx, &context.user_id, fallback.clone()).await?
                    } else {
                        return Err(e);
                    }
                }
            };

            // Convert UserAgent to AgentConfig
            let agent_config = AgentConfig {
                model: user_agent.model.clone(),
                system_prompt: user_agent.system_prompt.clone(),
                tools: user_agent.tools_vec(),
                max_tool_iterations: user_agent.max_tool_iterations as usize,
                parallel_tools: user_agent.parallel_tools,
                allowed_tools: None,
                extra: std::collections::HashMap::new(),
            };

            // Create the agent via AgentRegistryService via Context
            let mut agent = self.ctx.get::<ares_agents::AgentRegistry>().expect("AgentRegistry not provided")
                .create_agent_from_config_with_fallbacks(
                    &current_agent_name,
                    &agent_config,
                    &context.user_id,
                    &self.ctx.get::<crate::TenantDb>().expect("not provided").pool().clone(),
                    &self.ctx.get::<crate::context_services::FleetSecretsService>().expect("not provided").0,
                )
                .await?;
            agent.set_run_id(uuid::Uuid::new_v4().to_string());

            // Execute the agent
            let agent_resp = agent.execute(&current_input, context).await?;
            let output = agent_resp.content;
            let duration_ms = step_start.elapsed().as_millis() as u64;

            // Record this step
            steps.push(WorkflowStep {
                agent_name: current_agent_name.clone(),
                input: current_input.clone(),
                output: output.clone(),
                timestamp,
                duration_ms,
            });

            if !agents_used.contains(&current_agent_name) {
                agents_used.push(current_agent_name.clone());
            }

            // Check if the agent is a router and needs to delegate
            if agent.agent_type() == AgentType::Router {
                // Router's output should be an agent name
                // Use robust parsing to handle various output formats
                let next_agent = Self::parse_routing_decision(&output);

                if let Some(agent_name) = &next_agent {
                    // Validate the routed agent exists (check hierarchy) via Context
                    if resolve_agent(&self.ctx, &context.user_id, agent_name.clone())
                        .await
                        .is_ok()
                    {
                        current_agent_name = agent_name.clone();
                        // Keep the original user input for the routed agent
                        depth += 1;
                        continue;
                    }
                }

                // Agent not found or couldn't parse - try fallback
                if let Some(fallback) = &workflow.fallback_agent {
                    // Use fallback if routed agent doesn't exist
                    tracing::warn!(
                        "Routed agent '{:?}' not found or invalid, using fallback '{}'",
                        next_agent,
                        fallback
                    );
                    current_agent_name = fallback.clone();
                    depth += 1;
                    continue;
                } else {
                    // No fallback, return the router's output as final
                    break;
                }
            }

            // Non-router agent - this is the final response
            break;
        }

        // Build the final output
        let final_response = steps
            .last()
            .map(|s| s.output.clone())
            .unwrap_or_else(|| "No response generated".to_string());

        Ok(WorkflowOutput {
            final_response,
            steps_executed: steps.len(),
            agents_used,
            reasoning_path: steps,
        })
    }

    /// Get available workflow names via Context
    pub fn available_workflows(&self) -> Vec<String> {
        self.ctx.get::<crate::AresConfigManager>().expect("not provided")
            .config()
            .workflows
            .keys()
            .cloned()
            .collect()
    }

    /// Check if a workflow exists via Context
    pub fn has_workflow(&self, name: &str) -> bool {
        self.ctx.get::<crate::AresConfigManager>().expect("not provided")
            .config()
            .workflows
            .contains_key(name)
    }

    /// Get workflow configuration via Context
    pub fn get_workflow_config(&self, name: &str) -> Option<WorkflowConfig> {
        self.ctx.get::<crate::AresConfigManager>().expect("not provided")
            .config()
            .get_workflow(name)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ProviderRegistry;
    use crate::tools::registry::ToolRegistry;
    use crate::utils::toml_config::{
        AgentConfig, AresConfig, AuthConfig, DatabaseConfig, ModelConfig, ProviderConfig,
        RagConfig, ServerConfig,
    };
    use crate::{AgentRegistry, AresConfigManager, DynamicConfigManager};
    use std::collections::HashMap;
    use std::sync::{Arc, Once};

    static WF_LOAD_ENV: Once = Once::new();
    static WF_INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    static WF_DB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn workflow_test_db_url() -> String {
        WF_LOAD_ENV.call_once(|| {
            let _ = dotenvy::dotenv();
        });
        if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
            return url;
        }
        if let Ok(url) = std::env::var("DATABASE_URL") {
            if url.contains("/ares") && !url.contains("ares_test") {
                return url.replace("/ares", "/ares_test");
            }
            return url;
        }
        "postgres://dirmacs@localhost:5432/ares_test".to_string()
    }

    async fn create_workflow_test_db() -> Arc<dyn crate::db::traits::DatabaseClient> {
        let url = workflow_test_db_url();
        let db = crate::db::PostgresClient::new_remote(url, String::new())
            .await
            .expect("workflow test db");
        sqlx::migrate!("./migrations")
            .run(&db.pool)
            .await
            .expect("migrations");
        Arc::new(db)
    }

    fn create_test_config() -> AresConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "ollama-local".to_string(),
            ProviderConfig::OpenAI {
                api_key_env: "TEST_KEY".to_string(),
                api_base: "https://test.example.com/v1".to_string(),
                default_model: "ministral-3:3b".to_string(),
            },
        );

        let mut models = HashMap::new();
        models.insert(
            "default".to_string(),
            ModelConfig {
                provider: "ollama-local".to_string(),
                model: "ministral-3:3b".to_string(),
                temperature: 0.7,
                max_tokens: 512,
            },
        );

        let mut agents = HashMap::new();
        agents.insert(
            "router".to_string(),
            AgentConfig {
                model: "default".to_string(),
                system_prompt: Some("Route queries to the appropriate agent.".to_string()),
                tools: vec![],
                allowed_tools: None,
                max_tool_iterations: 1,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );
        agents.insert(
            "orchestrator".to_string(),
            AgentConfig {
                model: "default".to_string(),
                system_prompt: Some("Handle complex queries.".to_string()),
                tools: vec![],
                allowed_tools: None,
                max_tool_iterations: 10,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );
        agents.insert(
            "product".to_string(),
            AgentConfig {
                model: "default".to_string(),
                system_prompt: Some("Handle product queries.".to_string()),
                tools: vec![],
                allowed_tools: None,
                max_tool_iterations: 5,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );

        let mut workflows = HashMap::new();
        workflows.insert(
            "default".to_string(),
            WorkflowConfig {
                entry_agent: "router".to_string(),
                fallback_agent: Some("orchestrator".to_string()),
                max_depth: 3,
                max_iterations: 5,
                parallel_subagents: false,
            },
        );
        workflows.insert(
            "research".to_string(),
            WorkflowConfig {
                entry_agent: "orchestrator".to_string(),
                fallback_agent: None,
                max_depth: 3,
                max_iterations: 10,
                parallel_subagents: true,
            },
        );

        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            database: DatabaseConfig::default(),
            nvidia: None,
            config: crate::utils::toml_config::DynamicConfigPaths::default(),
            providers,
            models,
            tools: HashMap::new(),
            agents,
            workflows,
            rag: RagConfig::default(),
            billing: crate::utils::toml_config::BillingConfig::default(),
            skills: None,
        }
    }

    #[tokio::test]
    async fn test_workflow_engine_creation() {
        let config = Arc::new(create_test_config());
        let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_registry = Arc::new(AgentRegistry::from_config(
            &config,
            provider_registry.clone(),
            tool_registry.clone(),
        ));

        // Create a dummy AppState for testing
        let state: AppState = {
            let ctx = ares_cordis_core::Context::new_root();
            let config_manager = Arc::new(AresConfigManager::from_config((*config).clone()));
            ctx.provide_arc(config_manager.clone());
            ctx.provide(crate::context_services::ConfigManagerService(config_manager));
            ctx.provide(crate::context_services::DynamicConfigService(Arc::new(DynamicConfigManager::new(std::path::PathBuf::from("config/agents"), std::path::PathBuf::from("config/models"), std::path::PathBuf::from("config/tools"), std::path::PathBuf::from("config/workflows"), std::path::PathBuf::from("config/mcps"), false).unwrap())));
            let db_tmp = Arc::new(crate::db::PostgresClient::new_test());
            ctx.provide(crate::context_services::DbService(db_tmp.clone() as std::sync::Arc<dyn crate::db::traits::DatabaseClient>));
            let tenant_db = Arc::new(crate::db::TenantDb::new(db_tmp.clone()));
            ctx.provide_arc(tenant_db.clone());
            ctx.provide(crate::context_services::TenantDbService(tenant_db));
            ctx.provide_arc(Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")));
            ctx.provide_arc(provider_registry.clone());
            ctx.provide(crate::context_services::ProviderRegistryService(provider_registry.clone()));
            ctx.provide_arc(agent_registry.clone());
            ctx.provide(crate::context_services::ToolRegistryService(tool_registry.clone()));
            let auth_service = Arc::new(crate::auth::jwt::AuthService::new("secret".to_string(), 900, 604800));
            ctx.provide_arc(auth_service.clone());
            ctx.provide(crate::context_services::AuthServiceWrapper(auth_service));
            ctx.provide(crate::context_services::DeployRegistryService(crate::api::handlers::deploy::new_deploy_registry()));
            ctx.provide(crate::context_services::LoopRegistryService(crate::api::handlers::loops::LoopRegistry::new()));
            ctx.provide(crate::context_services::EmergencyStopService(Arc::new(std::sync::atomic::AtomicBool::new(false))));
            ctx.provide(crate::context_services::ContextProviderService(Arc::new(crate::agents::NoOpContextProvider) as std::sync::Arc<dyn crate::agents::context_provider::ContextProvider>));
            ctx.provide(crate::context_services::FleetSecretsService(ares_config::fleet_secrets::FleetSecrets::new()));
            ctx.provide(crate::context_services::RuntimeToolRegistryService(Arc::new(crate::RuntimeToolRegistry::new(db_tmp.pool.clone()))));
            ctx.provide(crate::context_services::ActiveRunsService(Arc::new(crate::active_runs::ActiveRuns::new())));
            ctx.provide(crate::context_services::SkillEngineService(Arc::new(crate::skill_engine::SkillEngine::new(db_tmp.pool.clone(), tool_registry.clone(), Arc::new(crate::RuntimeToolRegistry::new(db_tmp.pool.clone())), Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")), Arc::new(AresConfigManager::from_config((*config).clone()))))));
            ctx
        };

        let engine = WorkflowEngine::new(state);

        assert!(engine.has_workflow("default"));
        assert!(engine.has_workflow("research"));
        assert!(!engine.has_workflow("nonexistent"));
    }

    #[tokio::test]
    async fn test_available_workflows() {
        let config = Arc::new(create_test_config());
        let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_registry = Arc::new(AgentRegistry::from_config(
            &config,
            provider_registry.clone(),
            tool_registry.clone(),
        ));

        // Create a dummy AppState for testing
        let state: AppState = {
            let ctx = ares_cordis_core::Context::new_root();
            let config_manager = Arc::new(AresConfigManager::from_config((*config).clone()));
            ctx.provide_arc(config_manager.clone());
            ctx.provide(crate::context_services::ConfigManagerService(config_manager));
            ctx.provide(crate::context_services::DynamicConfigService(Arc::new(DynamicConfigManager::new(std::path::PathBuf::from("config/agents"), std::path::PathBuf::from("config/models"), std::path::PathBuf::from("config/tools"), std::path::PathBuf::from("config/workflows"), std::path::PathBuf::from("config/mcps"), false).unwrap())));
            let db_tmp = Arc::new(crate::db::PostgresClient::new_test());
            ctx.provide(crate::context_services::DbService(db_tmp.clone() as std::sync::Arc<dyn crate::db::traits::DatabaseClient>));
            let tenant_db = Arc::new(crate::db::TenantDb::new(db_tmp.clone()));
            ctx.provide_arc(tenant_db.clone());
            ctx.provide(crate::context_services::TenantDbService(tenant_db));
            ctx.provide_arc(Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")));
            ctx.provide_arc(provider_registry.clone());
            ctx.provide(crate::context_services::ProviderRegistryService(provider_registry.clone()));
            ctx.provide_arc(agent_registry.clone());
            ctx.provide(crate::context_services::ToolRegistryService(tool_registry.clone()));
            let auth_service = Arc::new(crate::auth::jwt::AuthService::new("secret".to_string(), 900, 604800));
            ctx.provide_arc(auth_service.clone());
            ctx.provide(crate::context_services::AuthServiceWrapper(auth_service));
            ctx.provide(crate::context_services::DeployRegistryService(crate::api::handlers::deploy::new_deploy_registry()));
            ctx.provide(crate::context_services::LoopRegistryService(crate::api::handlers::loops::LoopRegistry::new()));
            ctx.provide(crate::context_services::EmergencyStopService(Arc::new(std::sync::atomic::AtomicBool::new(false))));
            ctx.provide(crate::context_services::ContextProviderService(Arc::new(crate::agents::NoOpContextProvider) as std::sync::Arc<dyn crate::agents::context_provider::ContextProvider>));
            ctx.provide(crate::context_services::FleetSecretsService(ares_config::fleet_secrets::FleetSecrets::new()));
            ctx.provide(crate::context_services::RuntimeToolRegistryService(Arc::new(crate::RuntimeToolRegistry::new(db_tmp.pool.clone()))));
            ctx.provide(crate::context_services::ActiveRunsService(Arc::new(crate::active_runs::ActiveRuns::new())));
            ctx.provide(crate::context_services::SkillEngineService(Arc::new(crate::skill_engine::SkillEngine::new(db_tmp.pool.clone(), tool_registry.clone(), Arc::new(crate::RuntimeToolRegistry::new(db_tmp.pool.clone())), Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")), Arc::new(AresConfigManager::from_config((*config).clone()))))));
            ctx
        };

        let engine = WorkflowEngine::new(state);
        let workflows = engine.available_workflows();

        assert!(workflows.contains(&"default".to_string()));
        assert!(workflows.contains(&"research".to_string()));
    }

    #[tokio::test]
    async fn test_get_workflow_config() {
        let config = Arc::new(create_test_config());
        let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_registry = Arc::new(AgentRegistry::from_config(
            &config,
            provider_registry.clone(),
            tool_registry.clone(),
        ));

        // Create a dummy AppState for testing
        let state: AppState = {
            let ctx = ares_cordis_core::Context::new_root();
            let config_manager = Arc::new(AresConfigManager::from_config((*config).clone()));
            ctx.provide_arc(config_manager.clone());
            ctx.provide(crate::context_services::ConfigManagerService(config_manager));
            ctx.provide(crate::context_services::DynamicConfigService(Arc::new(DynamicConfigManager::new(std::path::PathBuf::from("config/agents"), std::path::PathBuf::from("config/models"), std::path::PathBuf::from("config/tools"), std::path::PathBuf::from("config/workflows"), std::path::PathBuf::from("config/mcps"), false).unwrap())));
            let db_tmp = Arc::new(crate::db::PostgresClient::new_test());
            ctx.provide(crate::context_services::DbService(db_tmp.clone() as std::sync::Arc<dyn crate::db::traits::DatabaseClient>));
            let tenant_db = Arc::new(crate::db::TenantDb::new(db_tmp.clone()));
            ctx.provide_arc(tenant_db.clone());
            ctx.provide(crate::context_services::TenantDbService(tenant_db));
            ctx.provide_arc(Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")));
            ctx.provide_arc(provider_registry.clone());
            ctx.provide(crate::context_services::ProviderRegistryService(provider_registry.clone()));
            ctx.provide_arc(agent_registry.clone());
            ctx.provide(crate::context_services::ToolRegistryService(tool_registry.clone()));
            let auth_service = Arc::new(crate::auth::jwt::AuthService::new("secret".to_string(), 900, 604800));
            ctx.provide_arc(auth_service.clone());
            ctx.provide(crate::context_services::AuthServiceWrapper(auth_service));
            ctx.provide(crate::context_services::DeployRegistryService(crate::api::handlers::deploy::new_deploy_registry()));
            ctx.provide(crate::context_services::LoopRegistryService(crate::api::handlers::loops::LoopRegistry::new()));
            ctx.provide(crate::context_services::EmergencyStopService(Arc::new(std::sync::atomic::AtomicBool::new(false))));
            ctx.provide(crate::context_services::ContextProviderService(Arc::new(crate::agents::NoOpContextProvider) as std::sync::Arc<dyn crate::agents::context_provider::ContextProvider>));
            ctx.provide(crate::context_services::FleetSecretsService(ares_config::fleet_secrets::FleetSecrets::new()));
            ctx.provide(crate::context_services::RuntimeToolRegistryService(Arc::new(crate::RuntimeToolRegistry::new(db_tmp.pool.clone()))));
            ctx.provide(crate::context_services::ActiveRunsService(Arc::new(crate::active_runs::ActiveRuns::new())));
            ctx.provide(crate::context_services::SkillEngineService(Arc::new(crate::skill_engine::SkillEngine::new(db_tmp.pool.clone(), tool_registry.clone(), Arc::new(crate::RuntimeToolRegistry::new(db_tmp.pool.clone())), Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")), Arc::new(AresConfigManager::from_config((*config).clone()))))));
            ctx
        };

        let engine = WorkflowEngine::new(state);

        let default_config = engine.get_workflow_config("default").unwrap();
        assert_eq!(default_config.entry_agent, "router");
        assert_eq!(
            default_config.fallback_agent,
            Some("orchestrator".to_string())
        );
        assert_eq!(default_config.max_depth, 3);

        let research_config = engine.get_workflow_config("research").unwrap();
        assert_eq!(research_config.entry_agent, "orchestrator");
        assert!(research_config.parallel_subagents);
    }

    #[test]
    fn test_workflow_output_serialization() {
        let output = WorkflowOutput {
            final_response: "Test response".to_string(),
            steps_executed: 2,
            agents_used: vec!["router".to_string(), "product".to_string()],
            reasoning_path: vec![
                WorkflowStep {
                    agent_name: "router".to_string(),
                    input: "What products do we have?".to_string(),
                    output: "product".to_string(),
                    timestamp: 1702500000,
                    duration_ms: 150,
                },
                WorkflowStep {
                    agent_name: "product".to_string(),
                    input: "What products do we have?".to_string(),
                    output: "Test response".to_string(),
                    timestamp: 1702500001,
                    duration_ms: 500,
                },
            ],
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("Test response"));
        assert!(json.contains("router"));
        assert!(json.contains("product"));

        let deserialized: WorkflowOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.steps_executed, 2);
    }

    fn test_agent_context() -> AgentContext {
        AgentContext {
            user_id: "user-1".to_string(),
            session_id: "session-1".to_string(),
            conversation_history: vec![],
            user_memory: None,
        }
    }

    fn create_test_config_with_ollama(base_url: &str) -> AresConfig {
        let mut config = create_test_config();
        if let Some(provider) = config.providers.get_mut("ollama-local") {
            if let ProviderConfig::OpenAI { api_base, .. } = provider {
                *api_base = base_url.to_string();
            }
        }
        config
    }

    async fn build_engine_from_config(config: AresConfig) -> WorkflowEngine {
        let config = Arc::new(config);
        let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_registry = Arc::new(AgentRegistry::from_config(
            &config,
            provider_registry.clone(),
            tool_registry.clone(),
        ));

        let state: AppState = {
            let ctx = ares_cordis_core::Context::new_root();
            let config_manager = Arc::new(AresConfigManager::from_config((*config).clone()));
            ctx.provide_arc(config_manager.clone());
            ctx.provide(crate::context_services::ConfigManagerService(config_manager));
            ctx.provide(crate::context_services::DynamicConfigService(Arc::new(DynamicConfigManager::new(std::path::PathBuf::from("config/agents"), std::path::PathBuf::from("config/models"), std::path::PathBuf::from("config/tools"), std::path::PathBuf::from("config/workflows"), std::path::PathBuf::from("config/mcps"), false).unwrap())));
            let db_tmp = Arc::new(crate::db::PostgresClient::new_test());
            ctx.provide(crate::context_services::DbService(db_tmp.clone() as std::sync::Arc<dyn crate::db::traits::DatabaseClient>));
            let tenant_db = Arc::new(crate::db::TenantDb::new(db_tmp.clone()));
            ctx.provide_arc(tenant_db.clone());
            ctx.provide(crate::context_services::TenantDbService(tenant_db));
            ctx.provide_arc(Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")));
            ctx.provide_arc(provider_registry.clone());
            ctx.provide(crate::context_services::ProviderRegistryService(provider_registry.clone()));
            ctx.provide_arc(agent_registry.clone());
            ctx.provide(crate::context_services::ToolRegistryService(tool_registry.clone()));
            let auth_service = Arc::new(crate::auth::jwt::AuthService::new("secret".to_string(), 900, 604800));
            ctx.provide_arc(auth_service.clone());
            ctx.provide(crate::context_services::AuthServiceWrapper(auth_service));
            ctx.provide(crate::context_services::DeployRegistryService(crate::api::handlers::deploy::new_deploy_registry()));
            ctx.provide(crate::context_services::LoopRegistryService(crate::api::handlers::loops::LoopRegistry::new()));
            ctx.provide(crate::context_services::EmergencyStopService(Arc::new(std::sync::atomic::AtomicBool::new(false))));
            ctx.provide(crate::context_services::ContextProviderService(Arc::new(crate::agents::NoOpContextProvider) as std::sync::Arc<dyn crate::agents::context_provider::ContextProvider>));
            ctx.provide(crate::context_services::FleetSecretsService(ares_config::fleet_secrets::FleetSecrets::new()));
            ctx.provide(crate::context_services::RuntimeToolRegistryService(Arc::new(crate::RuntimeToolRegistry::new(db_tmp.pool.clone()))));
            ctx.provide(crate::context_services::ActiveRunsService(Arc::new(crate::active_runs::ActiveRuns::new())));
            ctx.provide(crate::context_services::SkillEngineService(Arc::new(crate::skill_engine::SkillEngine::new(db_tmp.pool.clone(), tool_registry.clone(), Arc::new(crate::RuntimeToolRegistry::new(db_tmp.pool.clone())), Arc::new(crate::ConfigBasedLLMFactory::new(provider_registry.clone(), "default")), Arc::new(AresConfigManager::from_config((*config).clone()))))));
            ctx
        };

        WorkflowEngine::new(state)
    }

    fn mock_ollama_chat_response(content: &str) -> serde_json::Value {
        serde_json::json!({
            "model": "ministral-3:3b",
            "created_at": "2024-01-01T00:00:00Z",
            "message": {
                "role": "assistant",
                "content": content
            },
            "done": true
        })
    }

    #[test]
    fn test_parse_routing_decision_exact_match() {
        assert_eq!(
            WorkflowEngine::parse_routing_decision("product").as_deref(),
            Some("product")
        );
        assert_eq!(
            WorkflowEngine::parse_routing_decision("  SALES  ").as_deref(),
            Some("sales")
        );
    }

    #[test]
    fn test_parse_routing_decision_word_split() {
        assert_eq!(
            WorkflowEngine::parse_routing_decision("route to: product.").as_deref(),
            Some("product")
        );
        assert_eq!(
            WorkflowEngine::parse_routing_decision("invoice agent").as_deref(),
            Some("invoice")
        );
    }

    #[test]
    fn test_parse_routing_decision_substring_match() {
        assert_eq!(
            WorkflowEngine::parse_routing_decision("I would route this to finance").as_deref(),
            Some("finance")
        );
    }

    #[test]
    fn test_parse_routing_decision_none_for_unknown() {
        assert!(WorkflowEngine::parse_routing_decision("unknown-agent").is_none());
        assert!(WorkflowEngine::parse_routing_decision("").is_none());
    }

    #[tokio::test]
    async fn test_execute_workflow_unknown_name() {
        let engine = build_engine_from_config(create_test_config()).await;
        let err = engine
            .execute_workflow("missing", "hello", &test_agent_context())
            .await
            .expect_err("missing workflow");
        assert!(err.to_string().contains("missing"));
    }

    #[tokio::test]
    async fn test_execute_workflow_orchestrator_single_step() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_ollama_chat_response(
                    "Orchestrator handled the request",
                )),
            )
            .mount(&mock_server)
            .await;

        let engine =
            build_engine_from_config(create_test_config_with_ollama(&mock_server.uri())).await;
        let output = engine
            .execute_workflow("research", "complex task", &test_agent_context())
            .await
            .expect("workflow output");

        assert_eq!(output.steps_executed, 1);
        assert_eq!(output.agents_used, vec!["orchestrator".to_string()]);
        assert!(output.final_response.contains("Orchestrator"));
    }

    #[tokio::test]
    async fn test_execute_workflow_router_routes_to_product() {
        use std::sync::{Arc, Mutex};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

        struct SequentialResponder {
            responses: Arc<Mutex<Vec<String>>>,
        }

        impl Respond for SequentialResponder {
            fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                let mut queue = self.responses.lock().unwrap();
                let content = queue.remove(0);
                ResponseTemplate::new(200).set_body_json(mock_ollama_chat_response(&content))
            }
        }

        let mock_server = MockServer::start().await;
        let responses = Arc::new(Mutex::new(vec![
            "product".to_string(),
            "We sell widgets and gadgets".to_string(),
        ]));
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(SequentialResponder {
                responses: responses.clone(),
            })
            .mount(&mock_server)
            .await;

        let engine =
            build_engine_from_config(create_test_config_with_ollama(&mock_server.uri())).await;
        let output = engine
            .execute_workflow(
                "default",
                "What products do we have?",
                &test_agent_context(),
            )
            .await
            .expect("routed workflow");

        assert_eq!(output.steps_executed, 2);
        assert_eq!(
            output.agents_used,
            vec!["router".to_string(), "product".to_string()]
        );
        assert!(output.final_response.contains("widgets"));
    }

    #[tokio::test]
    async fn test_execute_workflow_router_invalid_route_uses_fallback() {
        use std::sync::{Arc, Mutex};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

        struct SequentialResponder {
            responses: Arc<Mutex<Vec<String>>>,
        }

        impl Respond for SequentialResponder {
            fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
                let mut queue = self.responses.lock().unwrap();
                let content = queue.remove(0);
                ResponseTemplate::new(200).set_body_json(mock_ollama_chat_response(&content))
            }
        }

        let mock_server = MockServer::start().await;
        let responses = Arc::new(Mutex::new(vec![
            "not-a-valid-xyz".to_string(),
            "Fallback orchestrator response".to_string(),
        ]));
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(SequentialResponder {
                responses: responses.clone(),
            })
            .mount(&mock_server)
            .await;

        let engine =
            build_engine_from_config(create_test_config_with_ollama(&mock_server.uri())).await;
        let output = engine
            .execute_workflow("default", "weird query", &test_agent_context())
            .await
            .expect("fallback workflow");

        assert!(output.agents_used.contains(&"router".to_string()));
        assert!(output.agents_used.contains(&"orchestrator".to_string()));
        assert!(output.final_response.contains("Fallback"));
    }
}
