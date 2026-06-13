//! # A.R.E.S - Agentic Retrieval Enhanced Server
//!
//! A production-grade agentic chatbot server built in Rust with multi-provider
//! LLM support, tool calling, RAG, MCP integration, and advanced research capabilities.
//!
//! ## Overview
//!
//! A.R.E.S can be used in two ways:
//!
//! 1. **As a standalone server** - Run the `ares-server` binary
//! 2. **As a library** - Import components into your own Rust project
//!
//! ## Quick Start (Library Usage)
//!
//! Add to your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! ares-server = "0.5"
//! ```
//!
//! ### Basic Example
//!
//! ```rust,ignore
//! use ares::{Provider, LLMClient};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create an Ollama provider
//!     let provider = Provider::Ollama {
//!         base_url: "http://localhost:11434".to_string(),
//!         model: "llama3.2:3b".to_string(),
//!     };
//!
//!     // Create a client and generate a response
//!     let client = provider.create_client().await?;
//!     let response = client.generate("Hello, world!").await?;
//!     println!("{}", response);
//!
//!     Ok(())
//! }
//! ```
//!
//! ### Using Tools
//!
//! ```rust,ignore
//! use ares::{ToolRegistry, tools::calculator::Calculator};
//! use std::sync::Arc;
//!
//! let mut registry = ToolRegistry::new();
//! registry.register(Arc::new(Calculator));
//!
//! // Tools can be used with LLM function calling
//! let tool_definitions = registry.definitions();
//! ```
//!
//! ### Multi-Turn Tool Calling with ToolCoordinator
//!
//! ```rust,ignore
//! use ares::llm::{Provider, ToolCoordinator, ToolCallingConfig};
//! use ares::tools::ToolRegistry;
//! use std::sync::Arc;
//!
//! let provider = Provider::from_env()?;
//! let client = provider.create_client().await?;
//! let registry = Arc::new(ToolRegistry::new());
//!
//! // Create a unified coordinator that works with any LLM provider
//! let coordinator = ToolCoordinator::new(client, registry, ToolCallingConfig::default());
//!
//! // Execute a tool-calling conversation
//! let result = coordinator.execute(Some("You are a helpful assistant."), "What is 25 * 4?").await?;
//! println!("Response: {}", result.content);
//! println!("Tool calls made: {}", result.tool_calls.len());
//! ```
//!
//! ### Configuration-Driven Setup
//!
//! ```rust,ignore
//! use ares::{AresConfigManager, AgentRegistry, ProviderRegistry, ToolRegistry};
//! use std::sync::Arc;
//!
//! // Load configuration from ares.toml
//! let config_manager = AresConfigManager::new("ares.toml")?;
//! let config = config_manager.config();
//!
//! // Create registries from configuration
//! let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
//! let tool_registry = Arc::new(ToolRegistry::with_config(&config));
//! let agent_registry = AgentRegistry::from_config(
//!     &config,
//!     provider_registry,
//!     tool_registry,
//! );
//! ```
//!
//! ## Feature Flags
//!
//! | Feature | Description |
//! |---------|-------------|
//! | `ollama` | Ollama local inference (default) |
//! | `openai` | OpenAI API support |
//! | `azure` | Azure AI Foundry OpenAI-compatible support |
//! | `llamacpp` | Direct GGUF model loading |
//! | `postgres` | PostgreSQL database (default) |
//! | `qdrant` | Qdrant vector database |
//! | `mcp` | Model Context Protocol support |
//!
//! ## Modules
//!
//! - [`agents`] - Agent framework for multi-agent orchestration
//! - [`api`] - REST API handlers and routes
//! - [`auth`] - JWT authentication and middleware
//! - [`db`] - Database abstraction (PostgreSQL)
//! - [`llm`] - LLM client implementations
//! - [`tools`] - Tool definitions and registry
//! - [`workflows`] - Declarative workflow engine
//! - [`types`] - Common types and error handling
//!
//! ## Architecture
//!
//! A.R.E.S uses a hybrid configuration system:
//!
//! - **TOML** (`ares.toml`): Infrastructure config (server, auth, providers)
//! - **TOON** (`config/*.toon`): Behavioral config (agents, models, tools)
//!
//! Both support hot-reloading for zero-downtime configuration changes.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]
#![warn(rustdoc::missing_crate_level_docs)]

/// AI agent orchestration and management.
pub mod agents { pub use ares_agents::*; }
/// HTTP API handlers and routes.
#[cfg(feature = "postgres")]
pub mod api;
/// Run observability and cost tracking.
#[cfg(feature = "postgres")]
pub mod active_runs;
pub mod observability;
/// Periodic health metrics aggregation job.
#[cfg(feature = "postgres")]
pub mod health_metrics_job;
/// Background cron scheduler for agent schedules.
#[cfg(feature = "postgres")]
pub mod scheduler;
/// Unified trigger execution engine.
#[cfg(feature = "postgres")]
pub mod trigger_engine;
/// Skill execution engine.
#[cfg(feature = "postgres")]
pub mod skill_engine;
/// Inter-agent pipeline execution engine.
#[cfg(feature = "postgres")]
pub mod pipeline_engine;
/// JWT authentication and middleware.
#[cfg(feature = "postgres")]
pub mod auth;
/// Command-line interface and scaffolding.
pub mod cli;
/// Database clients (Turso/SQLite, Qdrant).
pub mod db { pub use ares_db::*; }
/// LLM provider clients and abstractions.
pub mod llm;
/// Model Context Protocol (MCP) server integration.
#[cfg(feature = "mcp")]
pub mod mcp { pub use ares_mcp::*; }
/// Conversation memory and context management.
pub mod memory { pub use ares_agents::memory::*; }
/// Middleware for API key auth and usage tracking.
#[cfg(feature = "postgres")]
pub mod middleware;
/// Multi-tenant models (Tenant, ApiKey, Quota).
pub mod models;
/// Retrieval Augmented Generation (RAG) components.
pub mod rag;
/// Multi-agent research coordination.
#[cfg(feature = "postgres")]
pub mod research { pub use ares_agents::research::*; }
/// SKILL.md file discovery and loading (requires `skills` feature).
#[cfg(feature = "skills")]
pub mod skills;
/// Built-in tools (calculator, web search).
pub mod tools;
/// Core types (requests, responses, errors).
pub mod types;
/// Configuration utilities (TOML, TOON).
pub mod utils;
/// Workflow engine for agent orchestration.
#[cfg(feature = "postgres")]
pub mod workflows;

// Re-export commonly used types
pub use agents::{AgentRegistry, AgentRegistryBuilder};
#[cfg(feature = "postgres")]
pub use db::tenants::TenantDb;
#[cfg(feature = "postgres")]
pub use db::PostgresClient;
#[cfg(feature = "postgres")]
pub use db::fleet_provider_secrets::FleetProviderSecretsStore;
pub use llm::client::LLMClientFactoryTrait;
pub use llm::{
    ConfigBasedLLMFactory, LLMClient, LLMClientFactory, LLMResponse, NvidiaCatalogCache,
    Provider, ProviderRegistry,
};
pub use models::{ApiKey, Tenant, TenantContext, TenantQuota, TenantTier};
pub use ares_config::fleet_secrets::{FleetSecrets, MasterKey};
#[cfg(feature = "postgres")]
pub use observability::RunObservability;
#[cfg(feature = "postgres")]
pub use tools::runtime_registry::RuntimeToolRegistry;
pub use tools::registry::ToolRegistry;
pub use types::{AppError, ErrorCode, Result};
pub use utils::toml_config::{AresConfig, AresConfigManager};
pub use utils::toon_config::DynamicConfigManager;
#[cfg(feature = "postgres")]
pub use workflows::{WorkflowEngine, WorkflowOutput, WorkflowStep};

use sqlx::PgPool;
use std::sync::Arc;

/// Application state shared across handlers (requires postgres feature for full server)
#[cfg(feature = "postgres")]
use crate::auth::jwt::AuthService;

/// Application state shared across handlers
#[cfg(feature = "postgres")]
#[derive(Clone)]
pub struct AppState {
    /// TOML-based infrastructure configuration with hot-reload support
    pub config_manager: Arc<AresConfigManager>,
    /// TOON-based dynamic behavioral configuration with hot-reload support
    pub dynamic_config: Arc<DynamicConfigManager>,
    /// Database client
    pub db: Arc<dyn crate::db::traits::DatabaseClient>,
    /// Multi-tenant database
    pub tenant_db: Arc<TenantDb>,
    /// LLM client factory (config-based)
    pub llm_factory: Arc<ConfigBasedLLMFactory>,
    /// Provider registry for model/provider management
    pub provider_registry: Arc<ProviderRegistry>,
    /// Agent registry for creating config-driven agents
    pub agent_registry: Arc<AgentRegistry>,
    /// Tool registry for agent tools
    pub tool_registry: Arc<ToolRegistry>,
    /// Authentication service
    pub auth_service: Arc<AuthService>,
    /// MCP client registry for external services like Eruka
    #[cfg(feature = "mcp")]
    pub mcp_registry: Option<Arc<crate::mcp::McpRegistry>>,
    /// Deploy registry for tracking deployment operations
    pub deploy_registry: crate::api::handlers::deploy::DeployRegistry,
    /// Loop registry for tracking loop-mode agent lifecycle
    pub loop_registry: crate::api::handlers::loops::LoopRegistry,
    /// Emergency stop flag — when true, all agent requests are rejected with 503.
    /// Set/cleared via POST /api/admin/agents/emergency-stop.
    pub emergency_stop: Arc<std::sync::atomic::AtomicBool>,
    /// External context provider for agent calls.
    /// OSS: NoOpContextProvider. Managed: ErukaContextProvider (from dirmacs-core).
    pub context_provider: Arc<dyn crate::agents::context_provider::ContextProvider>,
    /// Fleet-wide provider API key & config overrides. Read by the catalog
    /// refresh path and the LLM factory; written by admin endpoints under
    /// `X-Admin-Secret`. Hot-swap is `Arc<ArcSwap<...>>` internally.
    pub fleet_secrets: FleetSecrets,
    /// Runtime tool registry with hot-reload support. Admin CRUD endpoints
    /// mutate the DB then call [`RuntimeToolRegistry::reload`] so agents
    /// see new tools without a restart.
    pub runtime_tool_registry: Arc<RuntimeToolRegistry>,
    /// Active runs currently in progress. Used for the live dashboard.
    pub active_runs: Arc<active_runs::ActiveRuns>,
    /// Skill execution engine with tool registry and LLM factory wired in.
    pub skill_engine: Arc<skill_engine::SkillEngine>,
}

/// Returns the base ARES router with all generic endpoints.
///
/// Extension crates can `.merge()` additional routes and `.layer()` middleware
/// on top of this to build managed platform binaries.
///
/// # Example
///
/// ```rust,ignore
/// use ares::{base_router, AppState};
///
/// let app = base_router(state.clone())
///     .merge(my_custom_routes(state.clone()))
///     .layer(my_custom_middleware());
/// ```
#[cfg(feature = "postgres")]
pub fn base_router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/health", axum::routing::get(|| async { "OK" }))
        .nest(
            "/api",
            api::routes::create_router(state.auth_service.clone(), state.tenant_db.clone()),
        )
        .with_state(state)
}

/// Resolve an abstract model tier to a concrete `(provider_name, model_name)` pair.
///
/// 1. Looks up `tenant_model_tiers` for the given tenant + tier.
/// 2. If no tenant-specific mapping exists, falls back to `config.models`
///    under the tier name.
/// 3. Returns `None` if neither source has an entry.
#[cfg(feature = "postgres")]
pub async fn resolve_model_tier(
    tenant_id: &str,
    tier_name: &str,
    pool: &PgPool,
    config: &AresConfig,
) -> Option<(String, String)> {
    let store = db::tenant_model_tiers::TenantModelTierStore::new(pool);
    if let Ok(Some(tier)) = store.get(tenant_id, tier_name).await {
        return Some((tier.provider_name, tier.model_name));
    }
    config.models.get(tier_name).map(|mc| {
        (mc.provider.clone(), mc.model.clone())
    })
}

/// Check for pipeline links after an agent run completes and trigger any
/// downstream agents whose source agent matches.
#[cfg(feature = "postgres")]
pub async fn trigger_pipelines(
    app_state: &Arc<AppState>,
    tenant_id: &str,
    source_agent: &str,
    source_output: &str,
) -> Result<Vec<String>> {
    crate::pipeline_engine::execute_pipeline(source_agent, source_output, tenant_id, app_state)
        .await
        .map_err(|e| crate::types::AppError::Internal(e))
}

#[cfg(all(test, feature = "postgres"))]
mod lib_tests {
    use super::*;
    use crate::agents::context_provider::NoOpContextProvider;
    use crate::api::handlers::{deploy, loops};
    use crate::auth::jwt::AuthService;
    use crate::db::tenants::TenantDb;
    use crate::utils::toml_config::{
        AgentConfig, AresConfig, AuthConfig, BillingConfig, DatabaseConfig,
        DynamicConfigPaths, ModelConfig, ProviderConfig, RagConfig, ServerConfig,
    };
    use crate::{
        AgentRegistry, AresConfigManager, ConfigBasedLLMFactory, DynamicConfigManager,
        ProviderRegistry, ToolRegistry,
    };
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn minimal_config() -> AresConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "p".into(),
            ProviderConfig::OpenAI {
                api_key_env: "TEST_KEY".into(),
                api_base: "https://test.example.com/v1".into(),
                default_model: "m".into(),
            },
        );
        let mut models = HashMap::new();
        models.insert(
            "default".into(),
            ModelConfig {
                provider: "p".into(),
                model: "m".into(),
                temperature: 0.7,
                max_tokens: 512,
            },
        );
        let mut agents = HashMap::new();
        agents.insert(
            "a".into(),
            AgentConfig {
                model: "default".into(),
                system_prompt: None,
                tools: vec![],
                allowed_tools: None,
                max_tool_iterations: 1,
                parallel_tools: false,
                extra: HashMap::new(),
            },
        );
        AresConfig {
            server: ServerConfig::default(),
            auth: AuthConfig {
                jwt_secret_env: "JWT_SECRET".into(),
                jwt_access_expiry: 900,
                jwt_refresh_expiry: 604800,
                api_key_env: "API_KEY".into(),
            },
            database: DatabaseConfig::default(),
            nvidia: None,
            config: DynamicConfigPaths::default(),
            providers,
            models,
            tools: HashMap::new(),
            agents,
            workflows: HashMap::new(),
            rag: RagConfig::default(),
            billing: BillingConfig::default(),
            #[cfg(feature = "skills")]
            skills: None,
        }
    }

    fn test_app_state() -> AppState {
        let config = minimal_config();
        let config_manager = Arc::new(AresConfigManager::from_config(config));
        let provider_registry = Arc::new(ProviderRegistry::from_config(&config_manager.config()));
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_registry = Arc::new(AgentRegistry::from_config(
            &config_manager.config(),
            provider_registry.clone(),
            tool_registry.clone(),
        ));
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let base = temp_dir.path();
        for sub in ["agents", "models", "tools", "workflows", "mcps"] {
            std::fs::create_dir_all(base.join(sub)).expect("mkdir");
        }
        let dynamic_config = Arc::new(
            DynamicConfigManager::new(
                base.join("agents"),
                base.join("models"),
                base.join("tools"),
                base.join("workflows"),
                base.join("mcps"),
                false,
            )
            .expect("dynamic config"),
        );
        std::mem::forget(temp_dir);

        let db = Arc::new(crate::db::PostgresClient::new_test());
        let tool_registry_for_skill = tool_registry.clone();
        let provider_registry_for_skill = provider_registry.clone();
        let config_manager_for_skill = config_manager.clone();
        AppState {
            config_manager,
            dynamic_config,
            db: db.clone(),
            tenant_db: Arc::new(TenantDb::new(db.clone())),
            llm_factory: Arc::new(ConfigBasedLLMFactory::new(
                provider_registry.clone(),
                "default",
            )),
            provider_registry,
            agent_registry,
            tool_registry,
            auth_service: Arc::new(AuthService::new(
                "test-secret-at-least-32-characters-long".into(),
                900,
                604800,
            )),
            deploy_registry: deploy::new_deploy_registry(),
            loop_registry: loops::LoopRegistry::new(),
            emergency_stop: Arc::new(AtomicBool::new(false)),
            context_provider: Arc::new(NoOpContextProvider),
            #[cfg(feature = "mcp")]
            mcp_registry: None,
            fleet_secrets: ares_config::fleet_secrets::FleetSecrets::new(),
            runtime_tool_registry: Arc::new(RuntimeToolRegistry::new(db.pool.clone())),
            active_runs: Arc::new(active_runs::ActiveRuns::new()),
            skill_engine: Arc::new(crate::skill_engine::SkillEngine::new(
                db.pool.clone(),
                tool_registry_for_skill,
                Arc::new(RuntimeToolRegistry::new(db.pool.clone())),
                Arc::new(ConfigBasedLLMFactory::new(provider_registry_for_skill, "default")),
                config_manager_for_skill,
            )),
        }
    }

    #[tokio::test]
    async fn base_router_serves_health_check() {
        let server =
            axum_test::TestServer::new(base_router(test_app_state())).expect("test server");
        let response = server.get("/health").await;
        response.assert_status_ok();
        response.assert_text("OK");
    }
}

