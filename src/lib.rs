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
//! use ares::tools::{Tool, Tools, calculator::Calculator};
//! use cordis::Context;
//! use std::sync::Arc;
//!
//! let tools = Tools::from_static([Arc::new(Calculator) as Arc<dyn Tool>]);
//! let ctx = Context::new_root();
//! ctx.provide(tools);
//! let tools = ctx.get::<Tools>().expect("Tools provided");
//! let tool_definitions = tools.list(&ctx);
//! ```
//!
//! ### Multi-Turn Tool Calling with ToolCoordinator
//!
//! ```rust,ignore
//! use ares::llm::{Provider, ToolCoordinator, ToolCallingConfig};
//! use ares::tools::{Tool, Tools};
//! use cordis::Context;
//! use std::sync::Arc;
//!
//! let provider = Provider::from_env()?;
//! let client = provider.create_client().await?;
//! let tools = Arc::new(Tools::from_static(Vec::<Arc<dyn Tool>>::new()));
//! let ctx = Context::new_root();
//!
//! // Create a unified coordinator that works with any LLM provider
//! let coordinator = ToolCoordinator::new(client, tools, ToolCallingConfig::default());
//!
//! // Execute a tool-calling conversation
//! let result = coordinator.execute(Some("You are a helpful assistant."), "What is 25 * 4?", &ctx).await?;
//! println!("Response: {}", result.content);
//! println!("Tool calls made: {}", result.tool_calls.len());
//! ```
//!
//! ### Configuration-Driven Setup
//!
//! ```rust,ignore
//! use ares::{AresConfigManager, ProviderRegistry};
//! use ares::tools::{Tool, Tools};
//! use cordis::Context;
//! use std::sync::Arc;
//!
//! // Load configuration from ares.toml
//! let config_manager = AresConfigManager::new("ares.toml")?;
//! let config = config_manager.config();
//!
//! let provider_registry = Arc::new(ProviderRegistry::from_config(&config));
//! let ctx = Context::new_root();
//! ctx.provide_arc(provider_registry);
//! ctx.provide(Tools::from_static(Vec::<Arc<dyn Tool>>::new()));
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
#![allow(missing_docs)]
#![allow(rustdoc::missing_crate_level_docs)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(clippy::redundant_closure)]
#![allow(unused_imports)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::option_as_ref_deref)]
#![allow(clippy::map_flatten)]
#![allow(clippy::for_kv_map)]
#![allow(clippy::doc_lazy_continuation)]
#![allow(clippy::should_implement_trait)]
#![allow(clippy::new_without_default)]
#![allow(clippy::trim_split_whitespace)]
#![allow(clippy::explicit_counter_loop)]
#![allow(clippy::unnecessary_sort_by)]
#![allow(clippy::empty_line_after_doc_comments)]
#![allow(unexpected_cfgs)]
#![allow(private_interfaces)]
#![allow(dead_code)]
#![allow(ambiguous_glob_reexports)]

/// AI agent orchestration and management.
pub mod agents { pub use ares_agent::*; }
/// HTTP API handlers and routes.
#[cfg(feature = "postgres")]
pub mod api;
/// Run observability and cost tracking.
#[cfg(feature = "postgres")]
pub mod active_runs;
#[cfg(feature = "postgres")]
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
pub mod db { pub use ares_store::*; }
/// LLM provider clients and abstractions.
pub mod llm;
/// Model Context Protocol (MCP) server integration.
#[cfg(feature = "mcp")]
pub mod mcp { pub use ares_mcp::*; }
/// In-process MCP [`AgentRunner`](ares_mcp::AgentRunner) over [`Execute`](ares_agent::execution::Execute).
#[cfg(all(feature = "postgres", feature = "mcp"))]
pub mod mcp_agent_runner;
#[cfg(feature = "postgres")]
pub mod execution_stack;
/// Conversation memory and context management.
pub mod memory { pub use ares_agent::memory::*; }
/// Middleware for API key auth and usage tracking.
#[cfg(feature = "postgres")]
pub mod middleware;
/// Multi-tenant models (Tenant, ApiKey, Quota).
pub mod models;
/// Retrieval Augmented Generation (RAG) components.
pub mod rag;
/// Multi-agent research coordination.
#[cfg(feature = "postgres")]
pub mod research { pub use ares_agent::research::*; }
/// SKILL.md file discovery and loading — runtime-gated via `SkillsService::check()` (was `#[cfg(feature = "skills")]`).
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
/// Cordis context services that live in the server crate (`EmergencyStop`).
#[cfg(feature = "postgres")]
pub mod context_services;

// Re-export commonly used types
pub use agents::{AgentRegistry, AgentRegistryBuilder};
#[cfg(all(feature = "postgres", feature = "mcp"))]
pub use mcp_agent_runner::ExecutionAgentRunner;
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
pub use tools::{Tool, Tools};
pub use types::{AppError, ErrorCode, Result};
pub use utils::toml_config::{AresConfig, AresConfigManager};
pub use utils::toon_config::DynamicConfigManager;
#[cfg(feature = "postgres")]
pub use workflows::{WorkflowEngine, WorkflowOutput, WorkflowStep};

#[cfg(feature = "postgres")]
use sqlx::PgPool;
use std::sync::Arc;

/// Application state shared across handlers (requires postgres feature for full server)
#[cfg(feature = "postgres")]
use crate::auth::jwt::AuthService;

/// Cordis context type. `AppState` is `Arc<Context>`; tests and callers
/// construct a root via `Context::new_root()` then `provide` services.
pub use cordis::Context;

#[cfg(not(feature = "postgres"))]
pub type AppState = std::sync::Arc<Context>;

#[cfg(feature = "postgres")]
pub type AppState = std::sync::Arc<Context>;

#[cfg(feature = "postgres")]
pub type CordisAppState = AppState;

/// Cordis routes that still need `AppState` (applied by [`build_router`] or `main`).
#[cfg(feature = "postgres")]
pub fn cordis_routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/health", axum::routing::get(|| async { "OK" }))
        .route(
            "/health/context",
            axum::routing::get(crate::api::handlers::health_context::health_context),
        )
}

/// Build router from Cordis `Context` — primary entry (Phase 2 step 12).
/// Reads services via `ctx.get::<...>()` + `State<Arc<Context>>` handlers.
#[cfg(feature = "postgres")]
pub fn build_router(ctx: AppState) -> axum::Router {
    let _events = ctx.get::<cordis::EventsService>();
    let _registry = ctx.get::<cordis::RegistryService>();
    let _exec = ctx.get::<ares_agent::Execute>();
    let _tool_svc = ctx.get::<ares_tools::Tools>();
    cordis_routes().with_state(ctx)
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
    app_state: &AppState,
    tenant_id: &str,
    source_agent: &str,
    source_output: &str,
) -> Result<Vec<String>> {
    crate::pipeline_engine::execute_pipeline(source_agent, source_output, tenant_id, app_state)
        .await
        .map_err(crate::types::AppError::Internal)
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
        AresConfigManager, ConfigBasedLLMFactory, DynamicConfigManager,
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
            skills: None,
        }
    }

    fn test_ctx() -> AppState {
        let config = minimal_config();
        let config_manager = Arc::new(AresConfigManager::from_config(config));
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let base = temp_dir.path();
        for sub in ["agents", "models", "tools", "workflows", "mcps"] {
            std::fs::create_dir_all(base.join(sub)).expect("mkdir");
        }
        let _dynamic_config = Arc::new(
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

        let ctx = cordis::Context::new_root();
        ctx.provide_arc(config_manager);
        ctx
    }

    #[tokio::test]
    async fn build_router_serves_health_check() {
        let server =
            axum_test::TestServer::new(build_router(test_ctx())).expect("test server");
        let response = server.get("/health").await;
        response.assert_status_ok();
        response.assert_text("OK");
    }

    #[tokio::test]
    async fn build_router_serves_health_context() {
        let server =
            axum_test::TestServer::new(build_router(test_ctx())).expect("test server");
        let response = server.get("/health/context").await;
        response.assert_status_ok();
    }

    #[cfg(feature = "hmr")]
    #[test]
    fn ares_server_hmr_feature_forwards_to_cordis_core() {
        // apply_plugin_so_if_dylib is reachable when the feature is on
        let ctx = cordis::Context::new_root();
        let applied = cordis::hmr::apply_plugin_so_if_dylib(&ctx, std::path::Path::new("README.md")).expect("non-lib");
        assert!(!applied);
    }
}

