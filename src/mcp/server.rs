//! ARES MCP Server Implementation
//!
//! This module provides an MCP server implementation using the `rmcp` crate,
//! exposing ARES operations as MCP tools for external clients.
//!
//! # Features
//!
//! Enable with the `mcp` feature flag:
//!
//! ```toml
//! ares = { version = "0.6", features = ["mcp"] }
//! ```
//!
//! # Tools
//!
//! - ares_list_agents  — list available agents
//! - ares_run_agent    — run an agent with a message
//! - ares_get_status   — check agent run status
//! - ares_deploy_agent — deploy a .toon config
//! - ares_get_usage    — check usage/quota
//! - eruka_read        — read context from Eruka
//! - eruka_write       — write context to Eruka
//! - eruka_search      — search Eruka knowledge base

use crate::db::tenants::TenantDb;
use crate::mcp::auth::{extract_api_key_from_env, validate_mcp_api_key, McpSession};
use crate::mcp::eruka_proxy::ErukaProxy;
use crate::mcp::tools::*;
use crate::mcp::usage::{check_quota, record_mcp_usage, McpOperation};
use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParam, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::stdio;
use rmcp::ServerHandler;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;

/// The ARES MCP Server.
///
/// This struct implements `ServerHandler` from rmcp, which means rmcp
/// will call its methods when MCP clients invoke tools.
///
/// Lifecycle:
/// 1. MCP client spawns the ARES binary with `--mcp` flag
/// 2. ARES reads ARES_API_KEY from env, validates it, creates McpSession
/// 3. rmcp handles JSON-RPC transport (stdio)
/// 4. Each tool call: validate quota → execute → record usage → return result
#[derive(Clone)]
pub struct AresMcpServer {
    /// Database for auth and queries
    tenant_db: Arc<TenantDb>,
    /// Database pool for raw queries (PgPool is Arc internally — cheap to clone)
    pool: sqlx::PgPool,
    /// Authenticated session (set after successful auth)
    session: Arc<RwLock<Option<McpSession>>>,
    /// Eruka proxy client for eruka_read/write/search tools
    eruka: Arc<ErukaProxy>,
    /// ARES API base URL for internal HTTP calls
    ares_api_url: String,
    /// HTTP client for calling ARES's own HTTP API
    http: reqwest::Client,
}

impl AresMcpServer {
    /// Creates a new AresMcpServer.
    ///
    /// # Arguments
    /// - `tenant_db`: Tenant database for auth and tenant queries
    /// - `pool`: PostgreSQL connection pool for raw queries
    /// - `ares_api_url`: Base URL of ARES HTTP API (e.g., "https://api.ares.dirmacs.com")
    /// - `eruka_api_url`: Base URL of Eruka API (e.g., "https://eruka.dirmacs.com")
    pub fn new(
        tenant_db: Arc<TenantDb>,
        pool: sqlx::PgPool,
        ares_api_url: &str,
        eruka_api_url: &str,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client for MCP server");

        Self {
            tenant_db,
            pool,
            session: Arc::new(RwLock::new(None)),
            eruka: Arc::new(ErukaProxy::new(eruka_api_url)),
            ares_api_url: ares_api_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    /// Authenticates the MCP connection.
    /// Called once at startup before any tool calls.
    pub async fn authenticate(&self) -> Result<(), String> {
        let api_key = extract_api_key_from_env()
            .map_err(|e| format!("MCP auth failed: {}", e))?;

        let tenant = validate_mcp_api_key(&self.tenant_db, &api_key)
            .await
            .map_err(|e| format!("MCP auth failed: {}", e))?;

        let session = McpSession::new(tenant, api_key);

        tracing::info!(
            tenant_id = session.tenant_id(),
            tier = session.tier(),
            "MCP session authenticated"
        );

        *self.session.write().await = Some(session);
        Ok(())
    }

    /// Gets the current session, or returns an error if not authenticated.
    async fn get_session(&self) -> Result<McpSession, String> {
        let session = self.session.read().await;
        session.clone().ok_or_else(|| "Not authenticated. Set ARES_API_KEY.".to_string())
    }

    /// Checks quota before executing a tool call.
    async fn enforce_quota(&self, session: &McpSession) -> Result<(), String> {
        let within_quota = check_quota(&self.pool, session.tenant_id(), session.tier())
            .await
            .map_err(|e| format!("Quota check failed: {}", e))?;

        if !within_quota {
            return Err(format!(
                "Usage quota exceeded for tier '{}'. Upgrade at https://dotdot.dirmacs.com/billing",
                session.tier()
            ));
        }

        Ok(())
    }

    /// Records usage after a tool call completes.
    async fn track_usage(
        &self,
        tenant_id: &str,
        operation: McpOperation,
        tokens: u64,
        success: bool,
        duration_ms: u64,
    ) {
        if let Err(e) = record_mcp_usage(
            &self.pool,
            tenant_id,
            operation,
            tokens,
            success,
            duration_ms,
        )
        .await
        {
            tracing::error!(
                error = %e,
                operation = operation.as_str(),
                "Failed to record MCP usage event — continuing anyway"
            );
        }
    }
}

// =============================================================================
// MCP Tool Implementations
// =============================================================================

impl AresMcpServer {
    /// List all agents available to the authenticated tenant.
    /// Returns agent names, descriptions, types, and deployment status.
    pub async fn list_agents(&self) -> Result<CallToolResult, String> {
        let start = std::time::Instant::now();
        let session = self.get_session().await?;
        
        // For now, return empty list - in production this would query the database
        let agents: Vec<AgentSummary> = Vec::new();
        let total = agents.len();
        
        let output = ListAgentsOutput { agents, total };
        let json = serde_json::to_string_pretty(&output)
            .unwrap_or_else(|_| "{}".to_string());

        let duration = start.elapsed().as_millis() as u64;
        self.track_usage(
            session.tenant_id(),
            McpOperation::ListAgents,
            0,
            true,
            duration,
        )
        .await;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Run an ARES agent with a message. Returns the agent's response.
    /// Optionally pass a context_id to continue an existing conversation.
    pub async fn run_agent(&self, input: RunAgentInput) -> Result<CallToolResult, String> {
        let start = std::time::Instant::now();
        let session = self.get_session().await?;
        self.enforce_quota(&session).await?;

        // Call ARES HTTP API: POST /api/chat
        let url = format!("{}/api/chat", self.ares_api_url);

        let mut body = serde_json::json!({
            "message": input.message,
            "agent_type": input.agent_name,
        });

        if let Some(ref ctx_id) = input.context_id {
            body["context_id"] = Value::String(ctx_id.clone());
        }

        let result = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", session.api_key))
            .json(&body)
            .send()
            .await;

        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(response) if response.status().is_success() => {
                let json: Value = response
                    .json()
                    .await
                    .map_err(|e| format!("Parse error: {}", e))?;

                let response_text = json["response"].as_str().unwrap_or("");
                let estimated_tokens = (response_text.len() / 4) as u64;

                self.track_usage(
                    session.tenant_id(),
                    McpOperation::RunAgent,
                    estimated_tokens,
                    true,
                    duration,
                )
                .await;

                let sources: Option<Vec<SourceRef>> = json["sources"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .map(|s| SourceRef {
                                title: s["title"].as_str().unwrap_or("").to_string(),
                                url: s["url"].as_str().map(String::from),
                                snippet: s["snippet"].as_str().map(String::from),
                            })
                            .collect()
                    });

                let output = RunAgentOutput {
                    response: response_text.to_string(),
                    agent: json["agent"].as_str().unwrap_or(&input.agent_name).to_string(),
                    context_id: json["context_id"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                    sources,
                };

                let output_json = serde_json::to_string_pretty(&output)
                    .unwrap_or_else(|_| "{}".to_string());

                Ok(CallToolResult::success(vec![Content::text(output_json)]))
            }
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::RunAgent,
                    0,
                    false,
                    duration,
                )
                .await;
                Err(format!("Agent run failed (HTTP {}): {}", status, body))
            }
            Err(e) => {
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::RunAgent,
                    0,
                    false,
                    duration,
                )
                .await;
                Err(format!("Failed to reach ARES API: {}", e))
            }
        }
    }

    /// Check the status of a previous agent run by context ID.
    pub async fn get_status(&self, input: GetStatusInput) -> Result<CallToolResult, String> {
        let start = std::time::Instant::now();
        let session = self.get_session().await?;

        let row = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            r#"
            SELECT status, partial_response, error_message
            FROM agent_runs
            WHERE context_id = $1 AND tenant_id = $2
            "#,
        )
        .bind(&input.context_id)
        .bind(session.tenant_id())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("DB error: {}", e))?;

        let duration = start.elapsed().as_millis() as u64;

        let output = match row {
            Some((status, partial, error)) => {
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::GetStatus,
                    0,
                    true,
                    duration,
                )
                .await;

                GetStatusOutput {
                    context_id: input.context_id,
                    status,
                    partial_response: partial,
                    error,
                }
            }
            None => {
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::GetStatus,
                    0,
                    true,
                    duration,
                )
                .await;

                GetStatusOutput {
                    context_id: input.context_id,
                    status: "not_found".to_string(),
                    partial_response: None,
                    error: None,
                }
            }
        };

        let json = serde_json::to_string_pretty(&output)
            .unwrap_or_else(|_| "{}".to_string());

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Deploy a new agent by uploading a .toon configuration.
    /// Pass the TOML config content as a string.
    pub async fn deploy_agent(&self, input: DeployAgentInput) -> Result<CallToolResult, String> {
        let start = std::time::Instant::now();
        let session = self.get_session().await?;
        self.enforce_quota(&session).await?;

        let url = format!("{}/api/user/agents/import", self.ares_api_url);

        let mut body = serde_json::json!({
            "config": input.toon_config,
            "format": "toon",
        });

        if let Some(name) = &input.name_override {
            body["name"] = Value::String(name.clone());
        }

        let result = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", session.api_key))
            .json(&body)
            .send()
            .await;

        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(response) if response.status().is_success() => {
                let json: Value = response
                    .json()
                    .await
                    .map_err(|e| format!("Parse error: {}", e))?;

                self.track_usage(
                    session.tenant_id(),
                    McpOperation::DeployAgent,
                    0,
                    true,
                    duration,
                )
                .await;

                let output = DeployAgentOutput {
                    agent_name: json["name"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string(),
                    action: json["action"]
                        .as_str()
                        .unwrap_or("created")
                        .to_string(),
                    active: json["active"].as_bool().unwrap_or(true),
                    deployed_at: json["deployed_at"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                };

                let output_json = serde_json::to_string_pretty(&output)
                    .unwrap_or_else(|_| "{}".to_string());

                Ok(CallToolResult::success(vec![Content::text(output_json)]))
            }
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::DeployAgent,
                    0,
                    false,
                    duration,
                )
                .await;
                Err(format!("Deploy failed (HTTP {}): {}", status, body))
            }
            Err(e) => {
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::DeployAgent,
                    0,
                    false,
                    duration,
                )
                .await;
                Err(format!("Failed to reach ARES API: {}", e))
            }
        }
    }

    /// Check your ARES usage statistics and quota.
    /// Optionally filter by date range.
    pub async fn get_usage(&self, input: GetUsageInput) -> Result<CallToolResult, String> {
        let start = std::time::Instant::now();
        let session = self.get_session().await?;

        let tenant_id = session.tenant_id().to_string();
        let tier = session.tier().to_string();

        let now = chrono::Utc::now();
        let from = input
            .from_date
            .unwrap_or_else(|| now.format("%Y-%m-01").to_string());
        let to = input
            .to_date
            .unwrap_or_else(|| now.format("%Y-%m-%d").to_string());

        let row: (i64, i64, i64) = sqlx::query_as(
            r#"
            SELECT
                COUNT(*) as total_requests,
                COALESCE(SUM(CASE WHEN operation LIKE 'mcp.%' THEN 1 ELSE 0 END), 0) as mcp_requests,
                COALESCE(SUM(effective_tokens), 0) as tokens_used
            FROM usage_events
            WHERE tenant_id = $1
              AND created_at >= $2
              AND created_at <= $3
            "#,
        )
        .bind(&tenant_id)
        .bind(&from)
        .bind(&to)
        .fetch_one(&self.pool)
        .await
        .unwrap_or((0, 0, 0));

        let agent_count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM user_agents WHERE tenant_id = $1",
        )
        .bind(&tenant_id)
        .fetch_one(&self.pool)
        .await
        .unwrap_or((0,));

        let duration = start.elapsed().as_millis() as u64;

        self.track_usage(
            &tenant_id,
            McpOperation::GetUsage,
            0,
            true,
            duration,
        )
        .await;

        let (max_requests, max_agents, max_tokens) = match tier.as_str() {
            "Free" => (1_000u64, 3u32, 10_000u64),
            "Dev" => (50_000, 20, 500_000),
            "Pro" => (500_000, 100, 5_000_000),
            "Enterprise" => (u64::MAX, u32::MAX, u64::MAX),
            _ => (1_000, 3, 10_000),
        };

        let tokens_used = row.2 as u64;
        let utilization = if max_tokens == u64::MAX {
            0.0
        } else {
            tokens_used as f64 / max_tokens as f64
        };

        let output = GetUsageOutput {
            tenant_id: tenant_id.clone(),
            tier: tier.clone(),
            period: UsagePeriod {
                from: from.clone(),
                to: to.clone(),
            },
            current_usage: UsageStats {
                total_requests: row.0 as u64,
                chat_requests: row.0 as u64 - row.1 as u64,
                mcp_requests: row.1 as u64,
                tokens_used,
                agents_deployed: agent_count.0 as u32,
            },
            quota: UsageQuota {
                max_requests_per_month: max_requests,
                max_agents,
                max_tokens_per_month: max_tokens,
                utilization,
            },
        };

        let json = serde_json::to_string_pretty(&output)
            .unwrap_or_else(|_| "{}".to_string());

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Read a knowledge field from Eruka.
    pub async fn eruka_read(&self, input: ErukaReadInput) -> Result<CallToolResult, String> {
        let start = std::time::Instant::now();
        let session = self.get_session().await?;
        self.enforce_quota(&session).await?;

        let result = self.eruka.read(&session, input).await;
        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(output) => {
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::ErukaRead,
                    0,
                    true,
                    duration,
                )
                .await;

                let json = serde_json::to_string_pretty(&output)
                    .unwrap_or_else(|_| "{}".to_string());

                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => {
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::ErukaRead,
                    0,
                    false,
                    duration,
                )
                .await;

                Err(format!("Eruka read failed: {}", e))
            }
        }
    }

    /// Write a knowledge field to Eruka.
    pub async fn eruka_write(&self, input: ErukaWriteInput) -> Result<CallToolResult, String> {
        let start = std::time::Instant::now();
        let session = self.get_session().await?;
        self.enforce_quota(&session).await?;

        let result = self.eruka.write(&session, input).await;
        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(output) => {
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::ErukaWrite,
                    0,
                    true,
                    duration,
                )
                .await;

                let json = serde_json::to_string_pretty(&output)
                    .unwrap_or_else(|_| "{}".to_string());

                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => {
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::ErukaWrite,
                    0,
                    false,
                    duration,
                )
                .await;

                Err(format!("Eruka write failed: {}", e))
            }
        }
    }

    /// Search Eruka knowledge base with a natural language query.
    pub async fn eruka_search(&self, input: ErukaSearchInput) -> Result<CallToolResult, String> {
        let start = std::time::Instant::now();
        let session = self.get_session().await?;
        self.enforce_quota(&session).await?;

        let result = self.eruka.search(&session, input).await;
        let duration = start.elapsed().as_millis() as u64;

        match result {
            Ok(output) => {
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::ErukaSearch,
                    0,
                    true,
                    duration,
                )
                .await;

                let json = serde_json::to_string_pretty(&output)
                    .unwrap_or_else(|_| "{}".to_string());

                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => {
                self.track_usage(
                    session.tenant_id(),
                    McpOperation::ErukaSearch,
                    0,
                    false,
                    duration,
                )
                .await;

                Err(format!("Eruka search failed: {}", e))
            }
        }
    }

    /// Get list of available tools with JSON schemas
    fn get_tools() -> Vec<Tool> {
        vec![
            Tool {
                name: "ares_list_agents".into(),
                description: Some(
                    "List all agents available in your ARES account. Returns agent names, descriptions, types, and deployment status.".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("List ARES Agents".into()),
            },
            Tool {
                name: "ares_run_agent".into(),
                description: Some(
                    "Run an ARES agent with a message. Specify the agent name and your message. Optionally pass a context_id to continue a conversation.".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "agent_name": {
                            "type": "string",
                            "description": "Name of the agent to run"
                        },
                        "message": {
                            "type": "string",
                            "description": "The message to send to the agent"
                        },
                        "context_id": {
                            "type": "string",
                            "description": "Optional context ID to continue a conversation"
                        }
                    },
                    "required": ["agent_name", "message"]
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Run ARES Agent".into()),
            },
            Tool {
                name: "ares_get_status".into(),
                description: Some(
                    "Check the status of a previous agent run. Pass the context_id from an ares_run_agent call. Returns running/completed/failed status.".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "context_id": {
                            "type": "string",
                            "description": "Context ID from a previous ares_run_agent call"
                        }
                    },
                    "required": ["context_id"]
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Get Agent Status".into()),
            },
            Tool {
                name: "ares_deploy_agent".into(),
                description: Some(
                    "Deploy a new agent to ARES by providing a .toon configuration (TOML format). The agent becomes immediately available for use.".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "toon_config": {
                            "type": "string",
                            "description": "The .toon config file contents as a string (TOML format)"
                        },
                        "name_override": {
                            "type": "string",
                            "description": "Optional: override the agent name from the config"
                        }
                    },
                    "required": ["toon_config"]
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Deploy Agent".into()),
            },
            Tool {
                name: "ares_get_usage".into(),
                description: Some(
                    "Check your ARES account usage statistics and quota. Shows requests made, tokens consumed, and remaining quota for your tier.".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "from_date": {
                            "type": "string",
                            "description": "Optional: filter by start date (ISO 8601, e.g. '2026-03-01')"
                        },
                        "to_date": {
                            "type": "string",
                            "description": "Optional: filter by end date (ISO 8601, e.g. '2026-03-31')"
                        }
                    },
                    "required": []
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Get Usage Stats".into()),
            },
            Tool {
                name: "eruka_read".into(),
                description: Some(
                    "Read a knowledge field from Eruka. Specify category (e.g., 'identity', 'market', 'content', 'products') and field name (e.g., 'company_name'). Returns the value, confidence, and knowledge state.".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "workspace_id": {
                            "type": "string",
                            "description": "Eruka workspace ID (defaults to tenant's workspace if omitted)"
                        },
                        "category": {
                            "type": "string",
                            "description": "Category to read from (e.g., 'identity', 'market', 'content')"
                        },
                        "field": {
                            "type": "string",
                            "description": "Specific field to read (e.g., 'company_name')"
                        }
                    },
                    "required": ["category", "field"]
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Eruka Read".into()),
            },
            Tool {
                name: "eruka_write".into(),
                description: Some(
                    "Write a knowledge field to Eruka. Provide category, field name, value, confidence score (0.0-1.0, use 1.0 for confirmed facts), and source description.".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "workspace_id": {
                            "type": "string",
                            "description": "Eruka workspace ID (defaults to tenant's workspace if omitted)"
                        },
                        "category": {
                            "type": "string",
                            "description": "Category to write to"
                        },
                        "field": {
                            "type": "string",
                            "description": "Field name"
                        },
                        "value": {
                            "type": "object",
                            "description": "Value to write (any JSON value)"
                        },
                        "confidence": {
                            "type": "number",
                            "description": "Confidence score (0.0 to 1.0, use 1.0 for user-confirmed facts)"
                        },
                        "source": {
                            "type": "string",
                            "description": "Source of the information (e.g., 'user_interview', 'web_research')"
                        }
                    },
                    "required": ["category", "field", "value", "confidence", "source"]
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Eruka Write".into()),
            },
            Tool {
                name: "eruka_search".into(),
                description: Some(
                    "Search the Eruka knowledge base with a natural language query. Returns matching fields with relevance scores.".into(),
                ),
                input_schema: serde_json::from_value(json!({
                    "type": "object",
                    "properties": {
                        "workspace_id": {
                            "type": "string",
                            "description": "Eruka workspace ID (defaults to tenant's workspace if omitted)"
                        },
                        "query": {
                            "type": "string",
                            "description": "Natural language search query"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results (default 5)",
                            "default": 5
                        }
                    },
                    "required": ["query"]
                }))
                .unwrap_or_default(),
                annotations: None,
                icons: None,
                meta: None,
                output_schema: None,
                title: Some("Eruka Search".into()),
            },
        ]
    }

    /// Execute a tool by name
    async fn execute_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> CallToolResult {
        let args = arguments.unwrap_or_default();
        let args_value = serde_json::Value::Object(args);

        let result = match name {
            "ares_list_agents" => self.list_agents().await,
            "ares_run_agent" => {
                match serde_json::from_value::<RunAgentInput>(args_value) {
                    Ok(input) => self.run_agent(input).await,
                    Err(e) => Err(format!("Invalid arguments: {}", e)),
                }
            }
            "ares_get_status" => {
                match serde_json::from_value::<GetStatusInput>(args_value) {
                    Ok(input) => self.get_status(input).await,
                    Err(e) => Err(format!("Invalid arguments: {}", e)),
                }
            }
            "ares_deploy_agent" => {
                match serde_json::from_value::<DeployAgentInput>(args_value) {
                    Ok(input) => self.deploy_agent(input).await,
                    Err(e) => Err(format!("Invalid arguments: {}", e)),
                }
            }
            "ares_get_usage" => {
                match serde_json::from_value::<GetUsageInput>(args_value) {
                    Ok(input) => self.get_usage(input).await,
                    Err(e) => Err(format!("Invalid arguments: {}", e)),
                }
            }
            "eruka_read" => {
                match serde_json::from_value::<ErukaReadInput>(args_value) {
                    Ok(input) => self.eruka_read(input).await,
                    Err(e) => Err(format!("Invalid arguments: {}", e)),
                }
            }
            "eruka_write" => {
                match serde_json::from_value::<ErukaWriteInput>(args_value) {
                    Ok(input) => self.eruka_write(input).await,
                    Err(e) => Err(format!("Invalid arguments: {}", e)),
                }
            }
            "eruka_search" => {
                match serde_json::from_value::<ErukaSearchInput>(args_value) {
                    Ok(input) => self.eruka_search(input).await,
                    Err(e) => Err(format!("Invalid arguments: {}", e)),
                }
            }
            _ => Err(format!("Unknown tool: {}", name)),
        };

        match result {
            Ok(call_result) => call_result,
            Err(e) => CallToolResult::error(vec![Content::text(e)]),
        }
    }
}

/// Implement ServerHandler for MCP protocol
impl ServerHandler for AresMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "A.R.E.S MCP Server - Provides ARES agent management and Eruka knowledge tools".into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult {
            tools: Self::get_tools(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(self.execute_tool(&request.name, request.arguments).await)
    }
}

// =============================================================================
// Server startup function
// =============================================================================

/// Starts the ARES MCP server in stdio mode.
///
/// This is called when the ARES binary is invoked with `--mcp` flag.
/// The server reads JSON-RPC messages from stdin and writes to stdout.
///
/// # Arguments
/// - `tenant_db`: Tenant database for auth
/// - `pool`: PostgreSQL connection pool
/// - `ares_api_url`: ARES HTTP API URL
/// - `eruka_api_url`: Eruka HTTP API URL
///
/// # Usage
/// ```bash
/// ARES_API_KEY=ares_abc123 ares --mcp
/// ```
pub async fn start_mcp_server(
    tenant_db: Arc<TenantDb>,
    pool: sqlx::PgPool,
    ares_api_url: &str,
    eruka_api_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let server = AresMcpServer::new(
        tenant_db,
        pool,
        ares_api_url,
        eruka_api_url,
    );

    // Authenticate before accepting tool calls
    server.authenticate().await?;

    tracing::info!("ARES MCP server starting on stdio transport");

    // Create stdio transport and run the server
    let transport = stdio();
    let server_handle = server.serve(transport).await?;

    // Wait for the server to finish (client disconnects or process exits)
    server_handle.waiting().await?;

    tracing::info!("ARES MCP server shut down");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_schemas() {
        let tools = AresMcpServer::get_tools();
        assert_eq!(tools.len(), 8);

        let tool_names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        assert!(tool_names.contains(&"ares_list_agents".to_string()));
        assert!(tool_names.contains(&"ares_run_agent".to_string()));
        assert!(tool_names.contains(&"ares_get_status".to_string()));
        assert!(tool_names.contains(&"ares_deploy_agent".to_string()));
        assert!(tool_names.contains(&"ares_get_usage".to_string()));
        assert!(tool_names.contains(&"eruka_read".to_string()));
        assert!(tool_names.contains(&"eruka_write".to_string()));
        assert!(tool_names.contains(&"eruka_search".to_string()));
    }
}
