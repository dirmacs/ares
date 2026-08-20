//! Agent execution service — single place handling conversation history loading,
//! memory injection, tool coordination, observability, usage/cost, token budget,
//! and loop detection.

use std::sync::Arc;

use ares_cordis_core::{Context, Service};
use ares_types::types::AppError;

use crate::AgentResponse;

/// Request for unified agent execution.
///
/// Carries the minimal fields needed to execute any agent via the single
/// `AgentExecutionService::execute` entry-point.
#[derive(Debug, Clone)]
pub struct AgentRequest {
    /// Agent name to execute.
    pub agent_name: String,
    /// User input / message.
    pub input: String,
    /// Session identifier for conversation history.
    pub session_id: String,
    /// User identifier.
    pub user_id: String,
    /// Tenant identifier (if multi-tenant).
    pub tenant_id: Option<String>,
}

impl Default for AgentRequest {
    fn default() -> Self {
        Self {
            agent_name: String::new(),
            input: String::new(),
            session_id: String::new(),
            user_id: String::new(),
            tenant_id: None,
        }
    }
}

/// Stub trait for unified tool resolution (Phase 5).
/// Real implementation will compose static + runtime + MCP bridge tools.
pub trait ToolService: Send + Sync + 'static {
    fn tool_names(&self) -> Vec<String>;
}

/// Unified agent execution service — the single place handling:
///
/// - conversation history loading (`TenantDb`)
/// - memory injection (`ContextProvider`)
/// - `ToolCoordinator` loop
/// - fallback LLM chain (`Coordinator`)
/// - observability sink (`run_history` + `agent_runs`)
/// - usage/cost aggregation
/// - token budget check
/// - loop detection
///
/// Reachable via `ctx.get::<AgentExecutionService>()` (see `Service` impl).
pub struct AgentExecutionService {
    // TODO: real fields (kept as comment to avoid hard deps before Phase 5):
    // db: Arc<dyn ares_db::traits::DatabaseClient>,
    // tenant_db: Arc<ares_db::tenants::TenantDb>,
    // llm_factory: Arc<ares_llm::provider_registry::ConfigBasedLLMFactory>,
    // tool_service: Option<Arc<dyn ToolService>>,
}

impl AgentExecutionService {
    /// Create a new stub service.
    pub fn new() -> Self {
        Self {}
    }

    // TODO: dedup from chat.rs:execute_agent, v1.rs:v1_chat, scheduler.rs:execute_scheduled_agent, pipeline_engine.rs:execute_target_agent, trigger_engine.rs:execute_triggered_agent
    /// Execute an agent request via the unified pathway.
    ///
    /// Currently a minimal skeleton that compiles; full implementation will
    /// handle conversation history loading (`TenantDb`), memory injection
    /// (`ContextProvider`), `ToolCoordinator` loop, fallback LLM chain
    /// (`Coordinator`), observability sink (`run_history` + `agent_runs`),
    /// usage/cost aggregation, token budget check, and loop detection.
    pub async fn execute(
        &self,
        _req: AgentRequest,
        _ctx: &Arc<Context>,
    ) -> Result<AgentResponse, AppError> {
        Ok(AgentResponse {
            content: "stub".into(),
            ..Default::default()
        })
    }
}

impl Default for AgentExecutionService {
    fn default() -> Self {
        Self::new()
    }
}

impl Service for AgentExecutionService {}
