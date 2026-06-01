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

use ares_db::tenants::TenantDb;
use crate::auth::{extract_api_key_from_env, validate_mcp_api_key, McpSession};
use crate::extension::{dispatch_extensions, McpToolExtension};
use crate::tools::*;
use crate::usage::{check_quota, record_mcp_usage, McpOperation};
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
    /// Extension tools registered by managed platform crates (e.g., Eruka tools from dirmacs-core)
    extensions: Vec<Arc<dyn McpToolExtension>>,
    /// ARES API base URL for internal HTTP calls
    ares_api_url: String,
    /// HTTP client for calling ARES's own HTTP API
    http: reqwest::Client,
    /// When true, `enforce_quota` is a no-op (unit tests only).
    #[cfg(test)]
    skip_quota_check: bool,
}

impl AresMcpServer {
    /// Creates a new AresMcpServer.
    ///
    /// # Arguments
    /// - `tenant_db`: Tenant database for auth and tenant queries
    /// - `pool`: PostgreSQL connection pool for raw queries
    /// - `ares_api_url`: Base URL of ARES HTTP API (e.g., "https://api.ares.dirmacs.com")
    pub fn new(
        tenant_db: Arc<TenantDb>,
        pool: sqlx::PgPool,
        ares_api_url: &str,
    ) -> Self {
        let extensions: Vec<Arc<dyn McpToolExtension>> = vec![];
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to build HTTP client for MCP server");

        Self {
            tenant_db,
            pool,
            session: Arc::new(RwLock::new(None)),
            extensions,
            ares_api_url: ares_api_url.trim_end_matches('/').to_string(),
            http,
            #[cfg(test)]
            skip_quota_check: false,
        }
    }

    /// Register an MCP tool extension. Extensions provide additional tools
    /// beyond the built-in ARES tools. Called by managed platform crates.
    pub fn register_extension(&mut self, ext: Arc<dyn McpToolExtension>) {
        self.extensions.push(ext);
    }

    /// Authenticates the MCP connection.
    /// Called once at startup before any tool calls.
    pub async fn authenticate(&self) -> Result<(), String> {
        let api_key = extract_api_key_from_env().map_err(|e| format!("MCP auth failed: {}", e))?;

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
        session
            .clone()
            .ok_or_else(|| "Not authenticated. Set ARES_API_KEY.".to_string())
    }

    /// Checks quota before executing a tool call.
    async fn enforce_quota(&self, session: &McpSession) -> Result<(), String> {
        #[cfg(test)]
        if self.skip_quota_check {
            let _ = session;
            return Ok(());
        }

        let within_quota = check_quota(&self.pool, session.tenant_id(), session.tier())
            .await
            .map_err(|e| format!("Quota check failed: {}", e))?;

        if !within_quota {
            return Err(format!(
                "Usage quota exceeded for tier '{}'. Contact your administrator to upgrade.",
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
        let json = serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string());

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

                let sources: Option<Vec<SourceRef>> = json["sources"].as_array().map(|arr| {
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
                    agent: json["agent"]
                        .as_str()
                        .unwrap_or(&input.agent_name)
                        .to_string(),
                    context_id: json["context_id"].as_str().unwrap_or("").to_string(),
                    sources,
                };

                let output_json =
                    serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string());

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

        let json = serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string());

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
                    agent_name: json["name"].as_str().unwrap_or("unknown").to_string(),
                    action: json["action"].as_str().unwrap_or("created").to_string(),
                    active: json["active"].as_bool().unwrap_or(true),
                    deployed_at: json["deployed_at"].as_str().unwrap_or("").to_string(),
                };

                let output_json =
                    serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string());

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
                COALESCE(SUM(CASE WHEN operation LIKE 'mcp.%' THEN 1 ELSE 0 END)::bigint, 0) as mcp_requests,
                COALESCE(SUM(effective_tokens)::bigint, 0) as tokens_used
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

        let agent_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM user_agents WHERE tenant_id = $1")
                .bind(&tenant_id)
                .fetch_one(&self.pool)
                .await
                .unwrap_or((0,));

        let duration = start.elapsed().as_millis() as u64;

        self.track_usage(&tenant_id, McpOperation::GetUsage, 0, true, duration)
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

        let json = serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string());

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }


    /// Get list of available tools with JSON schemas
    fn get_tools(&self) -> Vec<Tool> {
        let tools = vec![
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
        ];


        tools
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
            "ares_run_agent" => match serde_json::from_value::<RunAgentInput>(args_value) {
                Ok(input) => self.run_agent(input).await,
                Err(e) => Err(format!("Invalid arguments: {}", e)),
            },
            "ares_get_status" => match serde_json::from_value::<GetStatusInput>(args_value) {
                Ok(input) => self.get_status(input).await,
                Err(e) => Err(format!("Invalid arguments: {}", e)),
            },
            "ares_deploy_agent" => match serde_json::from_value::<DeployAgentInput>(args_value) {
                Ok(input) => self.deploy_agent(input).await,
                Err(e) => Err(format!("Invalid arguments: {}", e)),
            },
            "ares_get_usage" => match serde_json::from_value::<GetUsageInput>(args_value) {
                Ok(input) => self.get_usage(input).await,
                Err(e) => Err(format!("Invalid arguments: {}", e)),
            },
            // Try extension tools (eruka, custom tools from managed platform)
            other => {
                let tenant_id = match self.get_session().await {
                    Ok(s) => s.tenant_id().to_string(),
                    Err(e) => return CallToolResult::error(vec![Content::text(e)]),
                };
                if let Some(result) = dispatch_extensions(
                    &self.extensions,
                    other,
                    args_value.clone(),
                    &tenant_id,
                )
                .await
                {
                    return match result {
                        Ok(r) => r,
                        Err(e) => CallToolResult::error(vec![Content::text(e)]),
                    };
                }
                Err(format!("Unknown tool: {}", other))
            }
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
                "A.R.E.S MCP Server - Provides ARES agent management and Eruka knowledge tools"
                    .into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult {
            tools: self.get_tools(),
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

/// Starts the ARES MCP server in stdio mode.
///
/// This is called when the ARES binary is invoked with `--mcp` flag.
/// The server reads JSON-RPC messages from stdin and writes to stdout.
///
/// # Arguments
/// - `tenant_db`: Tenant database for auth
/// - `pool`: PostgreSQL connection pool
/// - `ares_api_url`: ARES HTTP API URL
///
/// Extension crates can register additional tools via `server.register_extension()`.
///
/// # Usage
/// ```bash
/// ARES_API_KEY=ares_abc123 ares --mcp
/// ```
pub async fn start_mcp_server(
    tenant_db: Arc<TenantDb>,
    pool: sqlx::PgPool,
    ares_api_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let server = AresMcpServer::new(tenant_db, pool, ares_api_url);

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
    use crate::extension::NoOpMcpExtension;
    use ares_db::postgres::PostgresClient;
    use ares_db::tenants::TenantDb;
    // TenantContext and TenantTier are available via super::* (server.rs uses them)
    use rmcp::ServerHandler;
    use std::sync::Arc;

    async fn test_server() -> AresMcpServer {
        let client = PostgresClient::new_test();
        let pool = client.pool.clone();
        let tenant_db = Arc::new(TenantDb::new(Arc::new(client)));
        AresMcpServer::new(tenant_db, pool, "https://api.test.com")
    }

    async fn test_server_with_url(url: &str) -> AresMcpServer {
        let client = PostgresClient::new_test();
        let pool = client.pool.clone();
        let tenant_db = Arc::new(TenantDb::new(Arc::new(client)));
        AresMcpServer::new(tenant_db, pool, url)
    }

    #[tokio::test]
    async fn new_trims_trailing_slash() {
        let server = test_server_with_url("https://api.test.com/").await;
        assert_eq!(server.ares_api_url, "https://api.test.com");
    }

    #[tokio::test]
    async fn new_preserves_url_without_slash() {
        let server = test_server_with_url("https://api.test.com").await;
        assert_eq!(server.ares_api_url, "https://api.test.com");
    }

    #[tokio::test]
    async fn register_extension_stores_it() {
        let mut server = test_server().await;
        assert!(server.extensions.is_empty());
        server.register_extension(Arc::new(NoOpMcpExtension));
        assert_eq!(server.extensions.len(), 1);
    }

    #[tokio::test]
    async fn get_tools_returns_five_with_correct_names() {
        let server = test_server().await;
        let tools = server.get_tools();
        assert_eq!(tools.len(), 5);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "ares_list_agents",
                "ares_run_agent",
                "ares_get_status",
                "ares_deploy_agent",
                "ares_get_usage",
            ]
        );
    }

    #[tokio::test]
    async fn execute_tool_unknown_name_returns_error() {
        let server = test_server().await;
        let result = server.execute_tool("nonexistent_tool", None).await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn execute_tool_run_agent_empty_args_returns_error() {
        let server = test_server().await;
        let result = server.execute_tool("ares_run_agent", Some(serde_json::Map::new())).await;
        assert_eq!(result.is_error, Some(true));
        let text = result.content.first().unwrap().as_text().unwrap().text.as_str();
        assert!(text.contains("Invalid arguments"));
    }

    #[tokio::test]
    async fn execute_tool_list_agents_without_session_returns_error() {
        let server = test_server().await;
        let result = server.execute_tool("ares_list_agents", None).await;
        assert_eq!(result.is_error, Some(true));
        let text = result.content.first().unwrap().as_text().unwrap().text.as_str();
        assert!(text.contains("Not authenticated"));
    }

    #[tokio::test]
    async fn execute_tool_get_status_without_session_returns_error() {
        let server = test_server().await;
        let args = serde_json::Map::from_iter(
            [("context_id".to_string(), serde_json::json!("ctx-1"))]
                .into_iter(),
        );
        let result = server.execute_tool("ares_get_status", Some(args)).await;
        assert_eq!(result.is_error, Some(true));
        let text = result.content.first().unwrap().as_text().unwrap().text.as_str();
        assert!(text.contains("Not authenticated"));
    }

    #[tokio::test]
    async fn execute_tool_get_usage_without_session_returns_error() {
        let server = test_server().await;
        let result = server.execute_tool("ares_get_usage", Some(serde_json::Map::new())).await;
        assert_eq!(result.is_error, Some(true));
        let text = result.content.first().unwrap().as_text().unwrap().text.as_str();
        assert!(text.contains("Not authenticated"));
    }

    #[tokio::test]
    async fn execute_tool_deploy_agent_without_session_returns_error() {
        let server = test_server().await;
        let args = serde_json::Map::from_iter(
            [("toon_config".to_string(), serde_json::json!("name = x"))]
                .into_iter(),
        );
        let result = server.execute_tool("ares_deploy_agent", Some(args)).await;
        assert_eq!(result.is_error, Some(true));
        let text = result.content.first().unwrap().as_text().unwrap().text.as_str();
        assert!(text.contains("Not authenticated"));
    }

    #[tokio::test]
    async fn get_tools_via_server_handler_returns_five() {
        let server = test_server().await;
        let tools = server.get_tools();
        assert_eq!(tools.len(), 5);
    }

    #[tokio::test]
    async fn get_info_via_server_handler_returns_valid_info() {
        let server = test_server().await;
        let info = server.get_info();
        assert_eq!(info.protocol_version, ProtocolVersion::V_2024_11_05);
        assert!(info.instructions.is_some());
        let instructions = info.instructions.as_deref().unwrap();
        assert!(instructions.contains("MCP Server"));
    }
    // =========================================================================
    // Serde roundtrip tests
    // =========================================================================

    #[test]
    fn run_agent_input_roundtrip_full() {
        let input = RunAgentInput {
            agent_name: "my-agent".into(),
            message: "hello world".into(),
            context_id: Some("ctx-42".into()),
        };
        let json = serde_json::to_string(&input).unwrap();
        let deserialized: RunAgentInput = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.agent_name, "my-agent");
        assert_eq!(deserialized.message, "hello world");
        assert_eq!(deserialized.context_id.as_deref(), Some("ctx-42"));
    }

    #[test]
    fn run_agent_input_roundtrip_no_context_id() {
        let json = r#"{"agent_name":"bot","message":"hi"}"#;
        let input: RunAgentInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.agent_name, "bot");
        assert_eq!(input.message, "hi");
        assert!(input.context_id.is_none());
        let json2 = serde_json::to_string(&input).unwrap();
        let input2: RunAgentInput = serde_json::from_str(&json2).unwrap();
        assert_eq!(input.agent_name, input2.agent_name);
        assert_eq!(input.message, input2.message);
        assert!(input2.context_id.is_none());
    }

    #[test]
    fn run_agent_input_deserialize_missing_required_field() {
        let json = r#"{"agent_name":"bot"}"#;
        let result = serde_json::from_str::<RunAgentInput>(json);
        assert!(result.is_err(), "missing 'message' should fail");
    }

    #[test]
    fn get_status_input_roundtrip() {
        let input = GetStatusInput {
            context_id: "ctx-99".into(),
        };
        let json = serde_json::to_string(&input).unwrap();
        let deserialized: GetStatusInput = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.context_id, "ctx-99");
    }

    #[test]
    fn get_status_input_deserialize_missing_context_id() {
        let json = r#"{}"#;
        let result = serde_json::from_str::<GetStatusInput>(json);
        assert!(result.is_err(), "missing 'context_id' should fail");
    }

    #[test]
    fn deploy_agent_input_roundtrip_with_override() {
        let input = DeployAgentInput {
            toon_config: "[agent]\nname = \"test\"".into(),
            name_override: Some("custom-name".into()),
        };
        let json = serde_json::to_string(&input).unwrap();
        let deserialized: DeployAgentInput = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.toon_config, "[agent]\nname = \"test\"");
        assert_eq!(deserialized.name_override.as_deref(), Some("custom-name"));
    }

    #[test]
    fn deploy_agent_input_roundtrip_no_override() {
        let json = r#"{"toon_config":"[agent]"}"#;
        let input: DeployAgentInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.toon_config, "[agent]");
        assert!(input.name_override.is_none());
    }

    #[test]
    fn deploy_agent_input_deserialize_missing_toon_config() {
        let json = r#"{"name_override":"x"}"#;
        let result = serde_json::from_str::<DeployAgentInput>(json);
        assert!(result.is_err(), "missing 'toon_config' should fail");
    }

    #[test]
    fn get_usage_input_default_is_empty() {
        let input = GetUsageInput::default();
        assert!(input.from_date.is_none());
        assert!(input.to_date.is_none());
    }

    #[test]
    fn get_usage_input_roundtrip_with_dates() {
        let input = GetUsageInput {
            from_date: Some("2026-01-01".into()),
            to_date: Some("2026-01-31".into()),
        };
        let json = serde_json::to_string(&input).unwrap();
        let deserialized: GetUsageInput = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.from_date.as_deref(), Some("2026-01-01"));
        assert_eq!(deserialized.to_date.as_deref(), Some("2026-01-31"));
    }

    #[test]
    fn get_usage_input_roundtrip_empty_object() {
        let json = r#"{}"#;
        let input: GetUsageInput = serde_json::from_str(json).unwrap();
        assert!(input.from_date.is_none());
        assert!(input.to_date.is_none());
    }

    #[test]
    fn agent_summary_roundtrip() {
        let agent = AgentSummary {
            name: "alpha".into(),
            description: "Test agent".into(),
            agent_type: "chat".into(),
            active: true,
            deployed_at: "2026-05-01T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&agent).unwrap();
        let deserialized: AgentSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "alpha");
        assert_eq!(deserialized.agent_type, "chat");
        assert!(deserialized.active);
    }

    #[test]
    fn source_ref_roundtrip_with_all_fields() {
        let source = SourceRef {
            title: "Doc".into(),
            url: Some("https://example.com".into()),
            snippet: Some("excerpt".into()),
        };
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: SourceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.title, "Doc");
        assert_eq!(deserialized.url.as_deref(), Some("https://example.com"));
        assert_eq!(deserialized.snippet.as_deref(), Some("excerpt"));
    }

    #[test]
    fn source_ref_roundtrip_optional_fields_none() {
        let source = SourceRef {
            title: "Bare".into(),
            url: None,
            snippet: None,
        };
        let json = serde_json::to_string(&source).unwrap();
        let deserialized: SourceRef = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.title, "Bare");
        assert!(deserialized.url.is_none());
        assert!(deserialized.snippet.is_none());
    }

    // =========================================================================
    // Constructor edge cases
    // =========================================================================

    #[tokio::test]
    async fn new_trims_multiple_trailing_slashes() {
        let server = test_server_with_url("https://api.test.com///").await;
        assert_eq!(server.ares_api_url, "https://api.test.com");
    }

    #[tokio::test]
    async fn new_empty_url_stays_empty() {
        let server = test_server_with_url("").await;
        assert_eq!(server.ares_api_url, "");
    }

    // =========================================================================
    // Tool schema validation
    // =========================================================================

    #[tokio::test]
    async fn tool_schemas_have_correct_required_fields() {
        let server = test_server().await;
        let tools = server.get_tools();

        let run_agent = tools.iter().find(|t| t.name == "ares_run_agent").unwrap();
        let required = run_agent.input_schema["required"].as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_names.contains(&"agent_name"));
        assert!(required_names.contains(&"message"));
        assert!(!required_names.contains(&"context_id"));

        let get_status = tools.iter().find(|t| t.name == "ares_get_status").unwrap();
        let required = get_status.input_schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str(), Some("context_id"));

        let deploy = tools.iter().find(|t| t.name == "ares_deploy_agent").unwrap();
        let required = deploy.input_schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str(), Some("toon_config"));

        for name in &["ares_list_agents", "ares_get_usage"] {
            let tool = tools.iter().find(|t| t.name == *name).unwrap();
            let required = tool.input_schema["required"].as_array().unwrap();
            assert!(required.is_empty(), "{} should have no required fields", name);
        }
    }

    #[tokio::test]
    async fn tool_schemas_have_object_type() {
        let server = test_server().await;
        let tools = server.get_tools();
        for tool in &tools {
            assert_eq!(
                tool.input_schema["type"].as_str(),
                Some("object"),
                "tool {} input_schema should have type 'object'",
                tool.name
            );
        }
    }

    #[tokio::test]
    async fn tool_schemas_have_descriptions() {
        let server = test_server().await;
        let tools = server.get_tools();
        for tool in &tools {
            assert!(
                tool.description.is_some(),
                "tool {} should have a description",
                tool.name
            );
            assert!(
                !tool.description.as_ref().unwrap().is_empty(),
                "tool {} description should not be empty",
                tool.name
            );
        }
    }

    #[tokio::test]
    async fn tool_schemas_have_titles() {
        let server = test_server().await;
        let tools = server.get_tools();
        for tool in &tools {
            assert!(
                tool.title.is_some(),
                "tool {} should have a title",
                tool.name
            );
        }
    }

    /// Helper: create a server with an injected session (bypasses auth).
    async fn test_server_with_session() -> AresMcpServer {
        let server = test_server().await;
        let tenant = ares_types::TenantContext::new(
            "test-tenant".into(),
            ares_types::TenantTier::Pro,
        );
        let session = crate::auth::McpSession::new(tenant, "test-api-key".into());
        *server.session.write().await = Some(session);
        server
    }
    // =========================================================================
    // Extension tool dispatch
    // =========================================================================

    use rmcp::model::Content as RmcpContent;

    struct TestExtensionSuccess;

    #[async_trait::async_trait]
    impl McpToolExtension for TestExtensionSuccess {
        fn tools(&self) -> Vec<rmcp::model::Tool> {
            vec![]
        }
        async fn execute(
            &self,
            tool_name: &str,
            _arguments: serde_json::Value,
            _tenant_id: &str,
        ) -> Option<Result<CallToolResult, String>> {
            if tool_name == "custom_tool" {
                Some(Ok(CallToolResult::success(vec![RmcpContent::text(
                    "custom result",
                )])))
            } else {
                None
            }
        }
    }

    struct TestExtensionError;

    #[async_trait::async_trait]
    impl McpToolExtension for TestExtensionError {
        fn tools(&self) -> Vec<rmcp::model::Tool> {
            vec![]
        }
        async fn execute(
            &self,
            tool_name: &str,
            _arguments: serde_json::Value,
            _tenant_id: &str,
        ) -> Option<Result<CallToolResult, String>> {
            if tool_name == "failing_tool" {
                Some(Err("extension failure".into()))
            } else {
                None
            }
        }
    }

    struct TestExtensionPassThrough;

    #[async_trait::async_trait]
    impl McpToolExtension for TestExtensionPassThrough {
        fn tools(&self) -> Vec<rmcp::model::Tool> {
            vec![]
        }
        async fn execute(
            &self,
            _tool_name: &str,
            _arguments: serde_json::Value,
            _tenant_id: &str,
        ) -> Option<Result<CallToolResult, String>> {
            None
        }
    }

    #[tokio::test]
    async fn execute_tool_extension_success() {
        let mut server = test_server_with_session().await;
        server.register_extension(Arc::new(TestExtensionSuccess));
        let result = server.execute_tool("custom_tool", None).await;
        assert_ne!(result.is_error, Some(true));
        let text = result.content.first().unwrap().as_text().unwrap().text.as_str();
        assert_eq!(text, "custom result");
    }

    #[tokio::test]
    async fn execute_tool_extension_error() {
        let mut server = test_server_with_session().await;
        server.register_extension(Arc::new(TestExtensionError));
        let result = server.execute_tool("failing_tool", None).await;
        assert_eq!(result.is_error, Some(true));
        let text = result.content.first().unwrap().as_text().unwrap().text.as_str();
        assert!(text.contains("extension failure"));
    }

    #[tokio::test]
    async fn execute_tool_extension_passthrough_falls_to_unknown() {
        let mut server = test_server_with_session().await;
        server.register_extension(Arc::new(TestExtensionPassThrough));
        let result = server.execute_tool("unknown_tool", None).await;
        assert_eq!(result.is_error, Some(true));
        let text = result.content.first().unwrap().as_text().unwrap().text.as_str();
        assert!(text.contains("Unknown tool"));
    }

    #[tokio::test]
    async fn execute_tool_first_extension_wins() {
        let mut server = test_server_with_session().await;
        server.register_extension(Arc::new(TestExtensionPassThrough));
        server.register_extension(Arc::new(TestExtensionSuccess));
        let result = server.execute_tool("custom_tool", None).await;
        assert_ne!(result.is_error, Some(true));
    }

    // =========================================================================
    // Execute tool with wrong JSON types
    // =========================================================================

    #[tokio::test]
    async fn execute_tool_run_agent_wrong_type_for_agent_name() {
        let server = test_server().await;
        let mut args = serde_json::Map::new();
        args.insert("agent_name".into(), serde_json::json!(12345));
        args.insert("message".into(), serde_json::json!("hello"));
        let result = server.execute_tool("ares_run_agent", Some(args)).await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn execute_tool_get_status_wrong_type_for_context_id() {
        let server = test_server().await;
        let mut args = serde_json::Map::new();
        args.insert("context_id".into(), serde_json::json!(42));
        let result = server.execute_tool("ares_get_status", Some(args)).await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn execute_tool_deploy_agent_wrong_type_for_toon_config() {
        let server = test_server().await;
        let mut args = serde_json::Map::new();
        args.insert("toon_config".into(), serde_json::json!(true));
        let result = server.execute_tool("ares_deploy_agent", Some(args)).await;
        assert_eq!(result.is_error, Some(true));
    }

    // =========================================================================
    // URL construction in tool dispatch
    // =========================================================================

    #[tokio::test]
    async fn api_url_no_double_slash_in_run_agent_path() {
        let server = test_server_with_url("https://api.test.com").await;
        assert_eq!(server.ares_api_url, "https://api.test.com");
        let url = format!("{}/api/chat", server.ares_api_url);
        assert_eq!(url, "https://api.test.com/api/chat");
    }

    #[tokio::test]
    async fn api_url_single_trailing_slash_in_deploy_path() {
        let server = test_server_with_url("https://api.test.com/").await;
        let url = format!("{}/api/user/agents/import", server.ares_api_url);
        assert_eq!(url, "https://api.test.com/api/user/agents/import");
    }

    // =========================================================================
    // Output struct serialization (Serialize-only types)
    // =========================================================================

    #[test]
    fn list_agents_output_serializes_correctly() {
        let output = ListAgentsOutput {
            agents: vec![],
            total: 0,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["total"], 0);
        assert!(json["agents"].as_array().unwrap().is_empty());
    }

    #[test]
    fn list_agents_output_with_agents() {
        let output = ListAgentsOutput {
            agents: vec![AgentSummary {
                name: "test".into(),
                description: "A test agent".into(),
                agent_type: "chat".into(),
                active: true,
                deployed_at: "2026-05-01".into(),
            }],
            total: 1,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["total"], 1);
        let agents = json["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["name"], "test");
    }

    #[test]
    fn run_agent_output_serializes_and_omits_none_sources() {
        let output = RunAgentOutput {
            response: "Hello".into(),
            agent: "bot".into(),
            context_id: "ctx-1".into(),
            sources: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["response"], "Hello");
        assert_eq!(json["agent"], "bot");
        assert_eq!(json["context_id"], "ctx-1");
        assert!(json.get("sources").is_none(), "None sources should be omitted");
    }

    #[test]
    fn run_agent_output_with_sources() {
        let output = RunAgentOutput {
            response: "Answer".into(),
            agent: "bot".into(),
            context_id: "ctx-2".into(),
            sources: Some(vec![SourceRef {
                title: "Wikipedia".into(),
                url: Some("https://en.wikipedia.org/wiki/Test".into()),
                snippet: None,
            }]),
        };
        let json = serde_json::to_value(&output).unwrap();
        let sources = json["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["title"], "Wikipedia");
        assert_eq!(sources[0].get("snippet"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn get_status_output_omits_none_fields() {
        let output = GetStatusOutput {
            context_id: "ctx-1".into(),
            status: "completed".into(),
            partial_response: None,
            error: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "completed");
        assert!(json.get("partial_response").is_none());
        assert!(json.get("error").is_none());
    }

    #[test]
    fn get_status_output_with_error() {
        let output = GetStatusOutput {
            context_id: "ctx-3".into(),
            status: "failed".into(),
            partial_response: Some("partial text".into()),
            error: Some("something went wrong".into()),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["status"], "failed");
        assert_eq!(json["partial_response"], "partial text");
        assert_eq!(json["error"], "something went wrong");
    }

    #[test]
    fn deploy_agent_output_serializes() {
        let output = DeployAgentOutput {
            agent_name: "new-agent".into(),
            action: "created".into(),
            active: true,
            deployed_at: "2026-05-31T12:00:00Z".into(),
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["agent_name"], "new-agent");
        assert_eq!(json["action"], "created");
        assert_eq!(json["active"], true);
    }

    #[test]
    fn get_usage_output_serializes_full() {
        let output = GetUsageOutput {
            tenant_id: "t-1".into(),
            tier: "Pro".into(),
            period: UsagePeriod {
                from: "2026-05-01".into(),
                to: "2026-05-31".into(),
            },
            current_usage: UsageStats {
                total_requests: 100,
                chat_requests: 80,
                mcp_requests: 20,
                tokens_used: 50000,
                agents_deployed: 3,
            },
            quota: UsageQuota {
                max_requests_per_month: 500_000,
                max_agents: 100,
                max_tokens_per_month: 5_000_000,
                utilization: 0.01,
            },
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["tenant_id"], "t-1");
        assert_eq!(json["tier"], "Pro");
        assert_eq!(json["period"]["from"], "2026-05-01");
        assert_eq!(json["current_usage"]["total_requests"], 100);
        assert_eq!(json["current_usage"]["mcp_requests"], 20);
        assert_eq!(json["quota"]["max_requests_per_month"], 500_000);
        assert_eq!(json["quota"]["utilization"], 0.01);
    }

    // =========================================================================
    // execute_tool: None arguments defaults to empty map
    // =========================================================================

    #[tokio::test]
    async fn execute_tool_none_arguments_handled_gracefully() {
        let server = test_server().await;
        let result = server.execute_tool("ares_list_agents", None).await;
        assert_eq!(result.is_error, Some(true));
        let text = result.content.first().unwrap().as_text().unwrap().text.as_str();
        assert!(text.contains("Not authenticated"));
    }

    // =========================================================================
    // ServerHandler get_info
    // =========================================================================

    #[tokio::test]
    async fn get_info_has_capabilities_with_tools() {
        let server = test_server().await;
        let info = server.get_info();
        let caps_json = serde_json::to_value(&info.capabilities).unwrap();
        assert!(caps_json.is_object());
    }

    #[tokio::test]
    async fn get_info_has_server_info_implementation() {
        let server = test_server().await;
        let info = server.get_info();
        let impl_json = serde_json::to_value(&info.server_info).unwrap();
        assert!(impl_json.is_object());
    }
    // =========================================================================
    // Auth lifecycle (authenticate)
    // =========================================================================

    use ares_types::TenantTier;
    use std::sync::Mutex;

    /// Serializes tests that mutate `ARES_API_KEY`.
    static AUTH_ENV_LOCK: Mutex<()> = Mutex::new(());

    async fn inject_session(server: &AresMcpServer, tier: TenantTier) {
        let tenant = ares_types::TenantContext::new("test-tenant".into(), tier);
        let session = crate::auth::McpSession::new(tenant, "ares_testkey12345678".into());
        *server.session.write().await = Some(session);
    }

    async fn test_server_with_session_tier(tier: TenantTier) -> AresMcpServer {
        let server = test_server().await;
        inject_session(&server, tier).await;
        server
    }

    async fn test_server_with_session_on_url(url: &str) -> AresMcpServer {
        let mut server = test_server_with_url(url).await;
        inject_session(&server, TenantTier::Pro).await;
        server.skip_quota_check = true;
        server
    }

    fn tool_result_text(result: &CallToolResult) -> &str {
        result.content.first().unwrap().as_text().unwrap().text.as_str()
    }

    #[tokio::test]
    async fn authenticate_missing_api_key_returns_error() {
        let _guard = AUTH_ENV_LOCK.lock().expect("auth env lock");
        std::env::remove_var("ARES_API_KEY");
        let server = test_server().await;
        let err = server.authenticate().await.unwrap_err();
        assert!(err.contains("MCP auth failed"));
        assert!(err.contains("No API key"));
        assert!(server.session.read().await.is_none());
    }

    #[tokio::test]
    async fn authenticate_invalid_key_format_returns_error() {
        let _guard = AUTH_ENV_LOCK.lock().expect("auth env lock");
        std::env::set_var("ARES_API_KEY", "not-a-valid-key");
        let server = test_server().await;
        let err = server.authenticate().await.unwrap_err();
        assert!(err.contains("MCP auth failed"));
        assert!(server.session.read().await.is_none());
    }

    #[tokio::test]
    async fn authenticate_valid_format_db_failure_returns_error() {
        let _guard = AUTH_ENV_LOCK.lock().expect("auth env lock");
        std::env::set_var("ARES_API_KEY", "ares_abcdefgh12345678");
        let server = test_server().await;
        let err = server.authenticate().await.unwrap_err();
        assert!(err.contains("MCP auth failed"));
        assert!(server.session.read().await.is_none());
    }

    // =========================================================================
    // Tool handlers with session (DB-less paths)
    // =========================================================================

    #[tokio::test]
    async fn list_agents_with_session_returns_empty_json() {
        let server = test_server_with_session_tier(TenantTier::Pro).await;
        let result = server.list_agents().await.expect("list_agents");
        assert_ne!(result.is_error, Some(true));
        let text = tool_result_text(&result);
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["total"], 0);
        assert!(parsed["agents"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_tool_list_agents_with_session_succeeds() {
        let server = test_server_with_session_tier(TenantTier::Pro).await;
        let result = server.execute_tool("ares_list_agents", None).await;
        assert_ne!(result.is_error, Some(true));
        let parsed: serde_json::Value = serde_json::from_str(tool_result_text(&result)).unwrap();
        assert_eq!(parsed["total"], 0);
    }

    #[tokio::test]
    async fn get_status_with_session_maps_db_error() {
        let server = test_server_with_session_tier(TenantTier::Pro).await;
        let input = GetStatusInput {
            context_id: "ctx-missing".into(),
        };
        let err = server.get_status(input).await.unwrap_err();
        assert!(err.starts_with("DB error:"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn get_usage_with_session_uses_default_period_and_tier_limits() {
        let server = test_server_with_session_tier(TenantTier::Pro).await;
        let result = server.get_usage(GetUsageInput::default()).await.expect("get_usage");
        let parsed: serde_json::Value = serde_json::from_str(tool_result_text(&result)).unwrap();
        assert_eq!(parsed["tenant_id"], "test-tenant");
        assert_eq!(parsed["tier"], "pro");
        assert_eq!(parsed["current_usage"]["total_requests"], 0);
        // Tier strings are lowercase from McpSession::tier(); match arms use PascalCase.
        assert_eq!(parsed["quota"]["max_requests_per_month"], 1_000);
        assert_eq!(parsed["quota"]["max_agents"], 3);
        assert_eq!(parsed["quota"]["max_tokens_per_month"], 10_000);
    }

    #[tokio::test]
    async fn get_usage_with_custom_dates_preserves_period() {
        let server = test_server_with_session_tier(TenantTier::Dev).await;
        let input = GetUsageInput {
            from_date: Some("2026-02-01".into()),
            to_date: Some("2026-02-28".into()),
        };
        let result = server.get_usage(input).await.expect("get_usage");
        let parsed: serde_json::Value = serde_json::from_str(tool_result_text(&result)).unwrap();
        assert_eq!(parsed["period"]["from"], "2026-02-01");
        assert_eq!(parsed["period"]["to"], "2026-02-28");
        assert_eq!(parsed["tier"], "dev");
    }

    #[tokio::test]
    async fn enforce_quota_reports_database_error_when_unavailable() {
        let server = test_server_with_session_tier(TenantTier::Free).await;
        let session = server.get_session().await.expect("session");
        let err = server.enforce_quota(&session).await.unwrap_err();
        assert!(err.starts_with("Quota check failed:"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn run_agent_without_skip_quota_reports_quota_check_failure() {
        let server = test_server_with_session_tier(TenantTier::Pro).await;
        let input = RunAgentInput {
            agent_name: "bot".into(),
            message: "hi".into(),
            context_id: None,
        };
        let err = server.run_agent(input).await.unwrap_err();
        assert!(err.starts_with("Quota check failed:"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn deploy_agent_without_skip_quota_reports_quota_check_failure() {
        let server = test_server_with_session_tier(TenantTier::Pro).await;
        let input = DeployAgentInput {
            toon_config: "[agent]".into(),
            name_override: None,
        };
        let err = server.deploy_agent(input).await.unwrap_err();
        assert!(err.starts_with("Quota check failed:"), "unexpected: {err}");
    }

    // =========================================================================
    // HTTP tool handlers (wiremock)
    // =========================================================================

    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn run_agent_success_returns_parsed_output() {
        let mock = MockServer::start().await;
        let body = serde_json::json!({
            "response": "Hello from agent",
            "agent": "support-bot",
            "context_id": "ctx-abc",
            "sources": [{
                "title": "Manual",
                "url": "https://example.com/manual",
                "snippet": "excerpt"
            }]
        });
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(header("authorization", "Bearer ares_testkey12345678"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&mock)
            .await;

        let base = format!("http://127.0.0.1:{}", mock.address().port());
        let server = test_server_with_session_on_url(&base).await;
        let result = server
            .run_agent(RunAgentInput {
                agent_name: "support-bot".into(),
                message: "hello".into(),
                context_id: Some("ctx-prev".into()),
            })
            .await
            .expect("run_agent");

        assert_ne!(result.is_error, Some(true));
        let parsed: serde_json::Value = serde_json::from_str(tool_result_text(&result)).unwrap();
        assert_eq!(parsed["response"], "Hello from agent");
        assert_eq!(parsed["agent"], "support-bot");
        assert_eq!(parsed["context_id"], "ctx-abc");
        let sources = parsed["sources"].as_array().unwrap();
        assert_eq!(sources[0]["title"], "Manual");
    }

    #[tokio::test]
    async fn run_agent_http_error_maps_status_and_body() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(503).set_body_string("service unavailable"),
            )
            .mount(&mock)
            .await;

        let base = format!("http://127.0.0.1:{}", mock.address().port());
        let server = test_server_with_session_on_url(&base).await;
        let err = server
            .run_agent(RunAgentInput {
                agent_name: "bot".into(),
                message: "ping".into(),
                context_id: None,
            })
            .await
            .unwrap_err();
        assert!(err.contains("Agent run failed (HTTP 503)"));
        assert!(err.contains("service unavailable"));
    }

    #[tokio::test]
    async fn run_agent_invalid_json_response_maps_parse_error() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
            .mount(&mock)
            .await;

        let base = format!("http://127.0.0.1:{}", mock.address().port());
        let server = test_server_with_session_on_url(&base).await;
        let err = server
            .run_agent(RunAgentInput {
                agent_name: "bot".into(),
                message: "ping".into(),
                context_id: None,
            })
            .await
            .unwrap_err();
        assert!(err.starts_with("Parse error:"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn run_agent_unreachable_api_maps_transport_error() {
        let server = test_server_with_session_on_url("http://127.0.0.1:1").await;
        let err = server
            .run_agent(RunAgentInput {
                agent_name: "bot".into(),
                message: "ping".into(),
                context_id: None,
            })
            .await
            .unwrap_err();
        assert!(err.starts_with("Failed to reach ARES API:"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn deploy_agent_success_with_name_override() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/user/agents/import"))
            .and(header("authorization", "Bearer ares_testkey12345678"))
            .and(body_json(serde_json::json!({
                "config": "[agent]\nname = \"from-config\"",
                "format": "toon",
                "name": "override-name"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "override-name",
                "action": "updated",
                "active": false,
                "deployed_at": "2026-06-01T00:00:00Z"
            })))
            .mount(&mock)
            .await;

        let base = format!("http://127.0.0.1:{}", mock.address().port());
        let server = test_server_with_session_on_url(&base).await;
        let result = server
            .deploy_agent(DeployAgentInput {
                toon_config: "[agent]\nname = \"from-config\"".into(),
                name_override: Some("override-name".into()),
            })
            .await
            .expect("deploy_agent");

        assert_ne!(result.is_error, Some(true));
        let parsed: serde_json::Value = serde_json::from_str(tool_result_text(&result)).unwrap();
        assert_eq!(parsed["agent_name"], "override-name");
        assert_eq!(parsed["action"], "updated");
        assert_eq!(parsed["active"], false);
    }

    #[tokio::test]
    async fn deploy_agent_http_error_maps_status() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/user/agents/import"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad config"))
            .mount(&mock)
            .await;

        let base = format!("http://127.0.0.1:{}", mock.address().port());
        let server = test_server_with_session_on_url(&base).await;
        let err = server
            .deploy_agent(DeployAgentInput {
                toon_config: "invalid".into(),
                name_override: None,
            })
            .await
            .unwrap_err();
        assert!(err.contains("Deploy failed (HTTP 400)"));
        assert!(err.contains("bad config"));
    }

    #[tokio::test]
    async fn execute_tool_run_agent_with_session_hits_mock_api() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "response": "ok",
                "agent": "bot",
                "context_id": "ctx-1"
            })))
            .mount(&mock)
            .await;

        let base = format!("http://127.0.0.1:{}", mock.address().port());
        let server = test_server_with_session_on_url(&base).await;
        let mut args = serde_json::Map::new();
        args.insert("agent_name".into(), serde_json::json!("bot"));
        args.insert("message".into(), serde_json::json!("hello"));
        let result = server.execute_tool("ares_run_agent", Some(args)).await;
        assert_ne!(result.is_error, Some(true));
        let parsed: serde_json::Value =
            serde_json::from_str(tool_result_text(&result)).unwrap();
        assert_eq!(parsed["response"], "ok");
    }

    // =========================================================================
    // ListAgentsInput and config edge cases
    // =========================================================================

    #[test]
    fn list_agents_input_default_roundtrip() {
        let input = ListAgentsInput::default();
        let json = serde_json::to_string(&input).unwrap();
        let back: ListAgentsInput = serde_json::from_str(&json).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), json);
    }

    #[test]
    fn new_clone_preserves_api_url_and_session_handle() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let a = test_server_with_url("https://api.example.com/").await;
            let b = a.clone();
            assert_eq!(a.ares_api_url, b.ares_api_url);
            assert!(Arc::ptr_eq(&a.session, &b.session));
        });
    }

}
