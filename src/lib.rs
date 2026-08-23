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
//! ares = "0.9"
//! ```
//!
//! ### Basic Example
//!
//! ```rust,ignore
//! use ares::{Context, Execute, Tools, Llm};
//! ```
//!
//! Run the `ares-server` binary for the HTTP product. The `ares` crate is the
//! library facade (no axum on the default graph).
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
pub use ares_http::api;
/// Run observability and cost tracking.
#[cfg(feature = "postgres")]
pub use ares_http::active_runs;
#[cfg(feature = "postgres")]
pub use ares_http::observability;
/// Periodic health metrics aggregation job.
#[cfg(feature = "postgres")]
pub mod health_metrics_job;
/// Background cron scheduler for agent schedules.
#[cfg(feature = "postgres")]
pub use ares_agent::scheduler;
/// Unified trigger execution engine.
#[cfg(feature = "postgres")]
pub use ares_agent::trigger;
/// Skill execution engine.
#[cfg(feature = "postgres")]
pub use ares_agent::skills;
#[cfg(feature = "postgres")]
pub use ares_agent::skills as skill_engine;
#[cfg(feature = "postgres")]
pub use ares_agent::skills::SkillEngine;
/// Inter-agent pipeline execution engine.
#[cfg(feature = "postgres")]
pub use ares_agent::pipeline;
/// JWT authentication and middleware.
#[cfg(feature = "postgres")]
pub use ares_http::auth;
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
/// Conversation memory and context management.
pub mod memory { pub use ares_agent::memory::*; }
/// Middleware for API key auth and usage tracking.
#[cfg(feature = "postgres")]
pub use ares_http::middleware;
/// Multi-tenant models (Tenant, ApiKey, Quota).
pub mod models;
/// Retrieval Augmented Generation (RAG) components.
pub mod rag;
/// Multi-agent research coordination.
#[cfg(feature = "postgres")]
pub mod research { pub use ares_agent::research::*; }
/// SKILL.md file discovery and loading — runtime-gated via `SkillsService::check()`.
#[cfg(any(feature = "postgres", feature = "skills"))]
pub use ares_agent::skills as skill_files;
/// Server Cordis loader factories (Overlay, Auth, engines, probe, Execute extras).
#[cfg(feature = "postgres")]
pub mod plugins;
#[cfg(feature = "postgres")]
pub use plugins::register_plugins;
/// Built-in tools (calculator, web search).
pub mod tools;
/// Core types (requests, responses, errors).
pub mod types;
/// Configuration overlay (`ares.toml` + TOON watches).
pub use ares_http::overlay;
/// TOON dynamic configuration loaded by Overlay.
pub use ares_http::toon_config;
/// HTTP auth/server config.
pub use ares_http::config as config_http;
/// Configuration utilities (TOML, TOON).
pub mod utils;
/// Workflow engine for agent orchestration.
#[cfg(feature = "postgres")]
pub use ares_agent::workflows;
/// Cordis context services (`EmergencyStop`).
pub use ares_agent::EmergencyStop;

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
// Residual: tests import ConfigBasedLLMFactory / ProviderRegistry at crate
// root (api_tests, integration_toml_tests, v1_tenant_agent_runtime_tests).
// NvidiaCatalogCache is not crate-root imported by binary or tests.
pub use llm::client::LLMClientFactoryTrait;
pub use llm::{
    ConfigBasedLLMFactory, LLMClient, LLMClientFactory, LLMResponse, Provider, ProviderRegistry,
};
pub use models::{ApiKey, QuotaExceeded, Tenant, TenantContext, TenantQuota, TenantTier};
pub use ares_store::{FleetSecrets, MasterKey};
#[cfg(feature = "postgres")]
pub use observability::RunObservability;
pub use tools::{Tool, Tools};
pub use types::{AppError, ErrorCode, Result};
pub use overlay::{AresConfig, AresConfigManager, Overlay, OverlayPlugin};
pub use overlay as toml_config;
pub use toon_config::DynamicConfigManager;
#[cfg(feature = "postgres")]
#[cfg(feature = "postgres")]
pub use ares_agent::workflows::{WorkflowEngine, WorkflowOutput, WorkflowStep};

#[cfg(feature = "postgres")]
use sqlx::PgPool;

/// Cordis context type.
pub use cordis::Context;
pub use ares_http::{Http, HttpPlugin, build_router, cordis_routes};

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
    app_state: &std::sync::Arc<Context>,
    tenant_id: &str,
    source_agent: &str,
    source_output: &str,
) -> Result<Vec<String>> {
    ares_agent::pipeline::execute_pipeline(source_agent, source_output, tenant_id, app_state)
        .await
        .map_err(crate::types::AppError::Internal)
}

#[cfg(all(test, feature = "postgres", feature = "hmr"))]
mod lib_tests {
    #[test]
    fn ares_server_hmr_feature_forwards_to_cordis_core() {
        // apply_plugin_so_if_dylib is reachable when the feature is on
        let ctx = cordis::Context::new_root();
        let applied = cordis::hmr::apply_plugin_so_if_dylib(&ctx, std::path::Path::new("README.md")).expect("non-lib");
        assert!(!applied);
    }
}
