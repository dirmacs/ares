//! Cordis Context service wrappers for legacy AppState fields.
//! Each wrapper holds the original AppState field type and implements `Service`
//! so `ctx.get::<Wrapper>().unwrap().0.clone()` retrieves the field via Context.

use std::sync::Arc;
use crate::AppState;
use std::sync::atomic::AtomicBool;

use crate::agents::context_provider::ContextProvider;
use crate::api::handlers::deploy::DeployRegistry;
use crate::api::handlers::loops::LoopRegistry;
use crate::auth::jwt::AuthService;
use crate::db::tenants::TenantDb;
use crate::db::traits::DatabaseClient;
use crate::{
    AgentRegistry, AresConfigManager, ConfigBasedLLMFactory, DynamicConfigManager, FleetSecrets,
    ProviderRegistry, RuntimeToolRegistry, ToolRegistry,
};
use ares_cordis_core::Service;

// Config
pub struct ConfigManagerService(pub Arc<AresConfigManager>);
impl Service for ConfigManagerService {}

pub struct DynamicConfigService(pub Arc<DynamicConfigManager>);
impl Service for DynamicConfigService {}

// DB
pub struct DbService(pub Arc<dyn DatabaseClient>);
impl Service for DbService {}

pub struct TenantDbService(pub Arc<TenantDb>);
impl Service for TenantDbService {}

// LLM — ConfigBasedLLMFactory now implements Service directly (ctx.get::<ConfigBasedLLMFactory>())
// AgentRegistry now implements Service directly (ctx.get::<AgentRegistry>())

pub struct ProviderRegistryService(pub Arc<ProviderRegistry>);
impl Service for ProviderRegistryService {}

pub struct ToolRegistryService(pub Arc<ToolRegistry>);
impl Service for ToolRegistryService {}

// Auth
pub struct AuthServiceWrapper(pub Arc<AuthService>);
impl Service for AuthServiceWrapper {}

// MCP optional
#[cfg(feature = "mcp")]
pub struct McpRegistryService(pub Option<Arc<crate::mcp::McpRegistry>>);
#[cfg(feature = "mcp")]
impl Service for McpRegistryService {}

// Deploy/Loop
pub struct DeployRegistryService(pub DeployRegistry);
impl Service for DeployRegistryService {}

pub struct LoopRegistryService(pub LoopRegistry);
impl Service for LoopRegistryService {}

// Emergency stop
pub struct EmergencyStopService(pub Arc<AtomicBool>);
impl Service for EmergencyStopService {}

// Context provider
pub struct ContextProviderService(pub Arc<dyn ContextProvider>);
impl Service for ContextProviderService {}

// Fleet secrets
pub struct FleetSecretsService(pub FleetSecrets);
impl Service for FleetSecretsService {}

// Runtime tools
pub struct RuntimeToolRegistryService(pub Arc<RuntimeToolRegistry>);
impl Service for RuntimeToolRegistryService {}

// Active runs
pub struct ActiveRunsService(pub Arc<crate::active_runs::ActiveRuns>);
impl Service for ActiveRunsService {}

// Skill engine
pub struct SkillEngineService(pub Arc<crate::skill_engine::SkillEngine>);
impl Service for SkillEngineService {}

// Postgres client direct (alternative for db)
pub struct PostgresClientService(pub Arc<crate::db::PostgresClient>);
impl Service for PostgresClientService {}
