//! Admin mcp domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use super::*;


use crate::AppState;
use crate::agents::context_provider::AgentRuntimeContext;
use crate::agents::tenant_agent;
use crate::db::agent_feedback;
use crate::db::agent_runs;
use crate::db::agent_versions;
use crate::db::alerts as db_alerts;
use crate::db::audit_log;
use crate::db::schedules as db_schedules;
use crate::db::skills as db_skills;
use crate::db::tenant_agents::{
    AgentTemplate, AgentTemplateStore, CreateTemplateRequest, CreateTenantAgentRequest,
    TenantAgent, UpdateTenantAgentRequest, clone_templates_for_tenant,
    create_tenant_agent as db_create_tenant_agent, delete_tenant_agent as db_delete_tenant_agent,
    get_tenant_agent as db_get_tenant_agent, list_agent_templates, list_tenant_agent_versions,
    list_tenant_agents as db_list_tenant_agents, record_tenant_agent_version,
    rollback_tenant_agent_version, update_tenant_agent as db_update_tenant_agent,
};
use crate::db::tenant_allowlist as allowlist;
use crate::db::tenant_model_tiers as db_tiers;
use crate::db::tenants::UsageSummary;
use crate::llm::provider_registry::{ModelInfo, RuntimeProviderEntry};
use crate::memory::estimate_tokens;
use crate::models::{Tenant, TenantTier};
use crate::types::{AgentContext, AppError, Result};
use crate::utils::toml_config::BillingConfig;
use ares_config::toml_config::ProviderConfig;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{Redirect, Response},
};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

pub async fn runtime_tool_capabilities() -> Json<RuntimeToolCapabilitiesResponse> {
    Json(RuntimeToolCapabilitiesResponse {
        tool_types: vec!["http", "mcp", "script", "sql"],
    })
}

pub fn routes() -> axum::Router<crate::AppState> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/mcp/runtime_tool_capabilities", get(runtime_tool_capabilities))
}

// TODO: ctx.plugin(AdminMcpRoutes, ...) — Service impl stub
// use ares_cordis_core::Service;
// pub struct AdminMcpService;
// impl Service for AdminMcpService {
//     fn name(&self) -> &'static str { "admin_mcp" }
//     fn check(&self) -> bool { true }
// }