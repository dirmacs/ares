// ares/src/mcp/tools.rs
// Input and output types for all ARES MCP tools.
// Each struct maps to one MCP tool's parameters or return value.

use serde::{Deserialize, Serialize};

// =============================================================================
// ares_list_agents
// =============================================================================

/// Input for ares_list_agents tool.
/// No parameters required — lists all agents for the authenticated tenant.
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct ListAgentsInput {}

/// One agent in the list response.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentSummary {
    /// Agent name (unique within tenant)
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Agent type: "chat", "workflow", "autonomous"
    pub agent_type: String,
    /// Whether the agent is currently active
    pub active: bool,
    /// When the agent was last deployed
    pub deployed_at: String,
}

/// Output for ares_list_agents tool.
#[derive(Debug, Serialize)]
pub struct ListAgentsOutput {
    pub agents: Vec<AgentSummary>,
    pub total: usize,
}

// =============================================================================
// ares_run_agent
// =============================================================================

/// Input for ares_run_agent tool.
#[derive(Debug, Deserialize, Serialize)]
pub struct RunAgentInput {
    /// Name of the agent to run (must exist in tenant's agent list)
    pub agent_name: String,
    /// The message to send to the agent
    pub message: String,
    /// Optional context ID for continuing a conversation
    #[serde(default)]
    pub context_id: Option<String>,
}

/// Output for ares_run_agent tool.
#[derive(Debug, Serialize)]
pub struct RunAgentOutput {
    /// The agent's response text
    pub response: String,
    /// Which agent handled the request
    pub agent: String,
    /// Context ID for continuing this conversation
    pub context_id: String,
    /// Sources cited by the agent (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<SourceRef>>,
}

/// A source reference from an agent response.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SourceRef {
    pub title: String,
    pub url: Option<String>,
    pub snippet: Option<String>,
}

// =============================================================================
// ares_get_status
// =============================================================================

/// Input for ares_get_status tool.
#[derive(Debug, Deserialize, Serialize)]
pub struct GetStatusInput {
    /// Context ID from a previous ares_run_agent call
    pub context_id: String,
}

/// Output for ares_get_status tool.
#[derive(Debug, Serialize)]
pub struct GetStatusOutput {
    pub context_id: String,
    /// "running", "completed", "failed", "not_found"
    pub status: String,
    /// Partial response text if still running
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_response: Option<String>,
    /// Error message if status is "failed"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// =============================================================================
// ares_deploy_agent
// =============================================================================

/// Input for ares_deploy_agent tool.
#[derive(Debug, Deserialize, Serialize)]
pub struct DeployAgentInput {
    /// The .toon config file contents as a string (TOML format)
    pub toon_config: String,
    /// Optional: override the agent name from the config
    #[serde(default)]
    pub name_override: Option<String>,
}

/// Output for ares_deploy_agent tool.
#[derive(Debug, Serialize)]
pub struct DeployAgentOutput {
    /// Name of the deployed agent
    pub agent_name: String,
    /// "created" or "updated"
    pub action: String,
    /// Whether the agent is now active
    pub active: bool,
    /// Deployment timestamp
    pub deployed_at: String,
}

// =============================================================================
// ares_get_usage
// =============================================================================

/// Input for ares_get_usage tool.
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct GetUsageInput {
    /// Optional: filter by date range (ISO 8601, e.g. "2026-03-01")
    #[serde(default)]
    pub from_date: Option<String>,
    #[serde(default)]
    pub to_date: Option<String>,
}

/// Output for ares_get_usage tool.
#[derive(Debug, Serialize)]
pub struct GetUsageOutput {
    pub tenant_id: String,
    pub tier: String,
    pub period: UsagePeriod,
    pub current_usage: UsageStats,
    pub quota: UsageQuota,
}

/// Usage period range.
#[derive(Debug, Serialize)]
pub struct UsagePeriod {
    pub from: String,
    pub to: String,
}

/// Current usage statistics.
#[derive(Debug, Serialize)]
pub struct UsageStats {
    pub total_requests: u64,
    pub chat_requests: u64,
    pub mcp_requests: u64,
    pub tokens_used: u64,
    pub agents_deployed: u32,
}

/// Quota limits for the tenant's tier.
#[derive(Debug, Serialize)]
pub struct UsageQuota {
    pub max_requests_per_month: u64,
    pub max_agents: u32,
    pub max_tokens_per_month: u64,
    /// Percentage of quota used (0.0 to 1.0)
    pub utilization: f64,
}

// =============================================================================
// eruka_read (proxy)
// =============================================================================

/// Input for eruka_read proxy tool.
#[derive(Debug, Deserialize, Serialize)]
pub struct ErukaReadInput {
    /// Eruka workspace ID (defaults to tenant's workspace if omitted)
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Category to read from (e.g. "identity", "market", "content")
    pub category: String,
    /// Specific field to read (e.g. "company_name")
    pub field: String,
}

/// Output for eruka_read proxy tool.
#[derive(Debug, Serialize)]
pub struct ErukaReadOutput {
    pub field: String,
    pub value: serde_json::Value,
    /// "CONFIRMED", "UNCERTAIN", "UNKNOWN"
    pub state: String,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
}

// =============================================================================
// eruka_write (proxy)
// =============================================================================

/// Input for eruka_write proxy tool.
#[derive(Debug, Deserialize, Serialize)]
pub struct ErukaWriteInput {
    /// Eruka workspace ID (defaults to tenant's workspace if omitted)
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Category to write to
    pub category: String,
    /// Field name
    pub field: String,
    /// Value to write (any JSON value)
    pub value: serde_json::Value,
    /// Confidence score (0.0 to 1.0, use 1.0 for user-confirmed facts)
    pub confidence: f64,
    /// Source of the information (e.g. "user_interview", "web_research")
    pub source: String,
}

/// Output for eruka_write proxy tool.
#[derive(Debug, Serialize)]
pub struct ErukaWriteOutput {
    pub field: String,
    /// "CONFIRMED" if confidence >= 1.0, "UNCERTAIN" otherwise
    pub state: String,
    pub written_at: String,
}

// =============================================================================
// eruka_search (proxy)
// =============================================================================

/// Input for eruka_search proxy tool.
#[derive(Debug, Deserialize, Serialize)]
pub struct ErukaSearchInput {
    /// Eruka workspace ID (defaults to tenant's workspace if omitted)
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Natural language search query
    pub query: String,
    /// Maximum number of results (default 5)
    #[serde(default = "default_search_limit")]
    pub limit: u32,
}

fn default_search_limit() -> u32 {
    5
}

/// One result from eruka_search.
#[derive(Debug, Serialize)]
pub struct ErukaSearchResult {
    pub category: String,
    pub field: String,
    pub value: serde_json::Value,
    pub state: String,
    pub relevance: f64,
}

/// Output for eruka_search proxy tool.
#[derive(Debug, Serialize)]
pub struct ErukaSearchOutput {
    pub results: Vec<ErukaSearchResult>,
    pub total_results: usize,
}
