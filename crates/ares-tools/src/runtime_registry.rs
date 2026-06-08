//! Runtime tool registry with hot-reload.
//!
//! Loads runtime tools from PostgreSQL via [`ares_db::runtime_tools::RuntimeToolStore`],
//! materialises them as `Arc<dyn Tool>` instances, and caches them in an
//! `ArcSwap<HashMap<String, Arc<dyn Tool>>>` for lock-free hot-swap reads.
//!
//! A background Tokio task can periodically call [`RuntimeToolRegistry::reload`] so
//! edits in the admin UI are reflected without a service restart.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
#[cfg(any(feature = "mcp", test))]
use async_trait::async_trait;
use serde_json::Value;
use tokio::time::interval;

use ares_db::runtime_tools::{RuntimeTool, RuntimeToolStore};
use ares_types::types::{AppError, Result, ToolDefinition};

use crate::http_tool::HttpTool;
use crate::registry::Tool;
use crate::script_tool::ScriptTool;

#[cfg(any(feature = "postgres", test))]
use crate::sql_tool::SqlTool;

// =============================================================================
// RuntimeMcpTool (feature-gated)
// =============================================================================

/// Configuration for a runtime MCP bridge tool.
#[cfg(any(feature = "mcp", test))]
#[derive(Debug, Clone, serde::Deserialize)]
struct RuntimeMcpToolConfig {
    #[serde(flatten)]
    server: ares_mcp::client::McpServerConfig,
    #[serde(default = "default_mcp_path")]
    path: String,
    #[serde(default = "default_mcp_method")]
    method: String,
}

#[cfg(any(feature = "mcp", test))]
fn default_mcp_path() -> String {
    "/api/v1/context".into()
}

#[cfg(any(feature = "mcp", test))]
fn default_mcp_method() -> String {
    "GET".into()
}

/// Runtime-configurable MCP bridge tool.
#[cfg(any(feature = "mcp", test))]
#[derive(Debug)]
pub struct RuntimeMcpTool {
    name: String,
    description: String,
    parameters_schema: Value,
    client: reqwest::Client,
    config: ares_mcp::client::McpServerConfig,
    default_path: String,
    default_method: ares_mcp::client::McpHttpMethod,
}

#[cfg(any(feature = "mcp", test))]
impl RuntimeMcpTool {
    /// Create a runtime MCP tool from server config and defaults.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: Value,
        server_config: ares_mcp::client::McpServerConfig,
        default_path: impl Into<String>,
        default_method: ares_mcp::client::McpHttpMethod,
    ) -> Result<Self> {
        let timeout = Duration::from_secs(
            server_config
                .timeout_secs
                .unwrap_or(ares_mcp::client::DEFAULT_CLIENT_TIMEOUT_SECS),
        );
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| AppError::External(format!("Failed to build HTTP client: {e}")))?;

        Ok(Self {
            name: name.into(),
            description: description.into(),
            parameters_schema,
            client,
            config: server_config,
            default_path: default_path.into(),
            default_method,
        })
    }

    /// Parse `execution_config` JSONB into server config + defaults.
    pub fn parse_config(
        execution_config: &Value,
    ) -> Result<(
        ares_mcp::client::McpServerConfig,
        String,
        ares_mcp::client::McpHttpMethod,
    )> {
        let cfg: RuntimeMcpToolConfig = serde_json::from_value(execution_config.clone())
            .map_err(|e| AppError::Configuration(format!("Invalid MCP tool config: {e}")))?;

        let method = match cfg.method.to_ascii_uppercase().as_str() {
            "GET" => ares_mcp::client::McpHttpMethod::Get,
            "POST" => ares_mcp::client::McpHttpMethod::Post,
            _ => {
                return Err(AppError::Configuration(format!(
                    "Invalid MCP method: {}",
                    cfg.method
                )))
            }
        };

        Ok((cfg.server, cfg.path, method))
    }
}

#[cfg(any(feature = "mcp", test))]
#[async_trait]
impl Tool for RuntimeMcpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.parameters_schema.clone()
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        if !self.config.enabled {
            return Err(AppError::Unavailable(format!(
                "MCP server '{}' is disabled",
                self.config.name
            )));
        }

        let endpoint = self
            .config
            .endpoint
            .as_deref()
            .ok_or_else(|| AppError::Configuration("MCP endpoint not configured".into()))?;

        let base_url = ares_mcp::client::normalize_endpoint_url(endpoint)
            .map_err(|e| AppError::Configuration(format!("Invalid MCP endpoint: {e}")))?;

        // Allow args to override path, method and body
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.default_path)
            .to_string();

        let method = args
            .get("method")
            .and_then(|v| v.as_str())
            .map(|m| match m.to_ascii_uppercase().as_str() {
                "GET" => Ok(ares_mcp::client::McpHttpMethod::Get),
                "POST" => Ok(ares_mcp::client::McpHttpMethod::Post),
                _ => Err(AppError::InvalidInput(format!("Invalid method: {m}"))),
            })
            .transpose()?
            .unwrap_or(self.default_method);

        let body = args.get("body").cloned();
        let query = args.get("query").and_then(|v| {
            v.as_object().map(|obj| {
                obj.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect::<Vec<_>>()
            })
        });

        let request = ares_mcp::client::McpClientRequest {
            method,
            path,
            query,
            body,
        };

        let built = ares_mcp::client::build_client_request(
            &base_url,
            &request,
            self.config.api_key.as_deref(),
        )
        .map_err(|e| AppError::External(format!("Failed to build MCP request: {e}")))?;

        let mut req = match built.method {
            ares_mcp::client::McpHttpMethod::Get => self.client.get(&built.url),
            ares_mcp::client::McpHttpMethod::Post => self.client.post(&built.url),
        };

        for (name, value) in &built.headers {
            req = req.header(name, value);
        }

        let resp = if let Some(body) = &built.body {
            req.json(body).send().await
        } else {
            req.send().await
        }
        .map_err(|e| AppError::External(format!("MCP request failed: {e}")))?;

        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();

        ares_mcp::client::parse_client_response(status, &text)
            .map_err(|e| AppError::External(format!("MCP response error: {e}")))
    }
}

// =============================================================================
// RuntimeToolRegistry
// =============================================================================

/// Thread-safe, hot-swappable registry of runtime tools loaded from the DB.
///
/// Internally stores an `Arc<ArcSwap<HashMap<String, Arc<dyn Tool>>>>` so
/// reads are lock-free and writes (reloads) are atomic.
pub struct RuntimeToolRegistry {
    pool: sqlx::PgPool,
    /// The tool cache -- `ArcSwap` for lock-free hot-swap reads.
    tools: Arc<ArcSwap<HashMap<String, Arc<dyn Tool>>>>,
    /// Metadata cache for tenant filtering and enablement checks.
    metadata: Arc<ArcSwap<HashMap<String, RuntimeTool>>>,
    /// Background reload interval in seconds. `0` disables background reload.
    reload_interval_secs: u64,
}

impl fmt::Debug for RuntimeToolRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeToolRegistry")
            .field("tool_count", &self.tools.load().len())
            .field("reload_interval_secs", &self.reload_interval_secs)
            .finish_non_exhaustive()
    }
}

impl Clone for RuntimeToolRegistry {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            tools: Arc::clone(&self.tools),
            metadata: Arc::clone(&self.metadata),
            reload_interval_secs: self.reload_interval_secs,
        }
    }
}

impl RuntimeToolRegistry {
    /// Create a new empty registry backed by `pool`.
    ///
    /// Call [`reload`] to populate the cache.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self::with_interval(pool, 60)
    }

    /// Create a registry with a custom background reload interval.
    ///
    /// `interval_secs == 0` disables background reload.
    pub fn with_interval(pool: sqlx::PgPool, interval_secs: u64) -> Self {
        Self {
            pool,
            tools: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            metadata: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            reload_interval_secs: interval_secs,
        }
    }

    /// Reload tools from the database.
    ///
    /// This fetches **all** runtime tools, materialises them, and atomically
    /// swaps the cache. Existing readers see the old map until the swap
    /// completes; new readers immediately see the new map.
    pub async fn reload(&self) -> Result<()> {
        let store = RuntimeToolStore::new(&self.pool);
        let rows = store.get_all().await?;
        self.build_and_swap(rows).await
    }

    /// Reload only tools visible to a given tenant (owned + public).
    pub async fn reload_for_tenant(&self, tenant_id: &str) -> Result<()> {
        let store = RuntimeToolStore::new(&self.pool);
        let rows = store.get_by_tenant(tenant_id).await?;
        self.build_and_swap(rows).await
    }

    async fn build_and_swap(&self, rows: Vec<RuntimeTool>) -> Result<()> {
        let mut tools = HashMap::with_capacity(rows.len());
        let mut metadata = HashMap::with_capacity(rows.len());

        for row in rows {
            let name = row.name.clone();
            if let Some(tool) = Self::materialise(&row) {
                tools.insert(name.clone(), tool);
            }
            metadata.insert(name, row);
        }

        self.tools.store(Arc::new(tools));
        self.metadata.store(Arc::new(metadata));

        Ok(())
    }

    /// Convert a [`RuntimeTool`] DB row into an `Arc<dyn Tool>`.
    ///
    /// Returns `None` if the tool type is unsupported, the config is invalid,
    /// or the tool is disabled.
    fn materialise(row: &RuntimeTool) -> Option<Arc<dyn Tool>> {
        if !row.enabled {
            return None;
        }

        let result = match row.tool_type.as_str() {
            "http" => Self::materialise_http(row),
            "script" => Self::materialise_script(row),
            "sql" => Self::materialise_sql(row),
            "mcp" => Self::materialise_mcp(row),
            _ => Err(AppError::Configuration(format!(
                "Unknown runtime tool type '{}'",
                row.tool_type
            ))),
        };

        match result {
            Ok(tool) => Some(tool),
            Err(_e) => None,
        }
    }

    fn materialise_http(row: &RuntimeTool) -> Result<Arc<dyn Tool>> {
        let config = HttpTool::parse_config(&row.execution_config)?;
        Ok(Arc::new(HttpTool::new(
            &row.name,
            &row.description,
            row.parameters_schema.clone(),
            config,
        )))
    }

    fn materialise_script(row: &RuntimeTool) -> Result<Arc<dyn Tool>> {
        let config = ScriptTool::parse_config(&row.execution_config)?;
        Ok(Arc::new(ScriptTool::new(
            &row.name,
            &row.description,
            row.parameters_schema.clone(),
            config,
        )))
    }

    #[cfg(any(feature = "postgres", test))]
    fn materialise_sql(row: &RuntimeTool) -> Result<Arc<dyn Tool>> {
        let config = SqlTool::parse_config(&row.execution_config)?;
        Ok(Arc::new(SqlTool::new(
            &row.name,
            &row.description,
            row.parameters_schema.clone(),
            config,
            None, // default_pool -- could be passed in constructor later
        )?))
    }

    #[cfg(not(any(feature = "postgres", test)))]
    fn materialise_sql(_row: &RuntimeTool) -> Result<Arc<dyn Tool>> {
        Err(AppError::FeatureDisabled(
            "SQL tools require postgres feature".into(),
        ))
    }

    #[cfg(any(feature = "mcp", test))]
    fn materialise_mcp(row: &RuntimeTool) -> Result<Arc<dyn Tool>> {
        let (server_config, path, method) = RuntimeMcpTool::parse_config(&row.execution_config)?;
        Ok(Arc::new(RuntimeMcpTool::new(
            &row.name,
            &row.description,
            row.parameters_schema.clone(),
            server_config,
            path,
            method,
        )?))
    }

    #[cfg(not(any(feature = "mcp", test)))]
    fn materialise_mcp(_row: &RuntimeTool) -> Result<Arc<dyn Tool>> {
        Err(AppError::FeatureDisabled(
            "MCP tools require mcp feature".into(),
        ))
    }

    // -------------------------------------------------------------------------
    // Read API (lock-free)
    // -------------------------------------------------------------------------

    /// Get a tool by name (fleet-wide, no tenant check).
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.load().get(name).cloned()
    }

    /// Get a tool by name, verifying tenant access.
    ///
    /// Returns `None` if the tool does not exist, is disabled, or the tenant
    /// does not have access to it.
    pub fn get_for_tenant(&self, name: &str, tenant_id: Option<&str>) -> Option<Arc<dyn Tool>> {
        let tools = self.tools.load();
        let meta = self.metadata.load();

        let tool = tools.get(name)?;
        let row = meta.get(name)?;

        if !row.enabled {
            return None;
        }

        if let Some(tid) = tenant_id {
            if !row.is_public && row.tenant_id.as_deref() != Some(tid) {
                return None;
            }
        }

        Some(Arc::clone(tool))
    }

    /// Check if a tool exists in the cache (fleet-wide, ignores tenant).
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.load().contains_key(name)
    }

    /// Execute a tool by name (fleet-wide, no tenant check).
    pub async fn execute(&self, name: &str, args: Value) -> Result<Value> {
        let tool = self
            .get(name)
            .ok_or_else(|| AppError::NotFound(format!("Runtime tool not found: {name}")))?;
        tool.execute(args).await
    }

    /// Execute a tool by name with tenant verification.
    pub async fn execute_for_tenant(
        &self,
        name: &str,
        args: Value,
        tenant_id: Option<&str>,
    ) -> Result<Value> {
        let tool = self.get_for_tenant(name, tenant_id).ok_or_else(|| {
            AppError::NotFound(format!(
                "Runtime tool not found or not accessible: {name}"
            ))
        })?;
        tool.execute(args).await
    }

    /// Get definitions for all enabled tools (fleet-wide).
    pub fn get_tool_definitions(&self) -> Vec<ToolDefinition> {
        let tools = self.tools.load();
        let meta = self.metadata.load();

        tools
            .values()
            .filter_map(|tool| {
                let row = meta.get(tool.name())?;
                if !row.enabled {
                    return None;
                }
                Some(ToolDefinition {
                    name: tool.name().to_string(),
                    description: row.description.clone(),
                    parameters: tool.parameters_schema(),
                })
            })
            .collect()
    }

    /// Get tool definitions scoped to a tenant.
    pub fn get_tool_definitions_for_tenant(
        &self,
        tenant_id: Option<&str>,
    ) -> Vec<ToolDefinition> {
        let tools = self.tools.load();
        let meta = self.metadata.load();

        tools
            .values()
            .filter_map(|tool| {
                let row = meta.get(tool.name())?;
                if !row.enabled {
                    return None;
                }
                if let Some(tid) = tenant_id {
                    if !row.is_public && row.tenant_id.as_deref() != Some(tid) {
                        return None;
                    }
                }
                Some(ToolDefinition {
                    name: tool.name().to_string(),
                    description: row.description.clone(),
                    parameters: tool.parameters_schema(),
                })
            })
            .collect()
    }

    /// Get definitions for specific tool names (fleet-wide, no tenant check).
    pub fn get_tool_definitions_for(&self, names: &[&str]) -> Vec<ToolDefinition> {
        let tools = self.tools.load();
        let meta = self.metadata.load();

        tools
            .values()
            .filter(|t| names.contains(&t.name()))
            .filter_map(|tool| {
                let row = meta.get(tool.name())?;
                if !row.enabled {
                    return None;
                }
                Some(ToolDefinition {
                    name: tool.name().to_string(),
                    description: row.description.clone(),
                    parameters: tool.parameters_schema(),
                })
            })
            .collect()
    }

    /// Get all enabled tool names visible to a tenant.
    pub fn enabled_tool_names(&self, tenant_id: Option<&str>) -> Vec<String> {
        let meta = self.metadata.load();

        meta.values()
            .filter(|row| {
                if !row.enabled {
                    return false;
                }
                if let Some(tid) = tenant_id {
                    row.is_public || row.tenant_id.as_deref() == Some(tid)
                } else {
                    true
                }
            })
            .map(|row| row.name.clone())
            .collect()
    }

    // -------------------------------------------------------------------------
    // Background reload
    // -------------------------------------------------------------------------

    /// Spawn a background Tokio task that calls [`reload`] periodically.
    ///
    /// Does nothing if `reload_interval_secs` is `0`.
    pub fn start_background_reload(self: Arc<Self>) {
        let secs = self.reload_interval_secs;
        if secs == 0 {
            return;
        }

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if let Err(_e) = self.reload().await {
                    // Silently ignore background reload errors to avoid
                    // spamming logs in environments without tracing.
                }
            }
        });
    }

    /// Atomically replace the reload interval.
    pub fn set_reload_interval(&mut self, secs: u64) {
        self.reload_interval_secs = secs;
    }

    /// Number of tools currently cached.
    pub fn len(&self) -> usize {
        self.tools.load().len()
    }

    /// Return `true` if no tools are cached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -------------------------------------------------------------------------
    // Mock tool
    // -------------------------------------------------------------------------

    struct MockTool {
        name: String,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "mock"
        }

        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }

        async fn execute(&self, args: Value) -> Result<Value> {
            Ok(json!({"tool": self.name, "args": args}))
        }
    }

    // -------------------------------------------------------------------------
    // ArcSwap smoke test
    // -------------------------------------------------------------------------

    #[test]
    fn arcswap_registry_pattern_works() {
        let mut map: HashMap<String, Arc<dyn Tool>> = HashMap::new();
        map.insert(
            "mock".into(),
            Arc::new(MockTool {
                name: "mock".into(),
            }) as Arc<dyn Tool>,
        );
        let swap = ArcSwap::from_pointee(map);
        assert_eq!(swap.load().len(), 1);
        assert!(swap.load().contains_key("mock"));
    }

    // -------------------------------------------------------------------------
    // Materialisation tests
    // -------------------------------------------------------------------------

    fn sample_runtime_tool(name: &str, tool_type: &str, enabled: bool) -> RuntimeTool {
        RuntimeTool {
            id: format!("uuid-{name}"),
            name: name.into(),
            display_name: None,
            description: format!("Test {name}"),
            tool_type: tool_type.into(),
            parameters_schema: json!({"type": "object"}),
            execution_config: json!({}),
            enabled,
            version: 1,
            is_public: true,
            created_by: None,
            tenant_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn materialise_http_tool() {
        let mut row = sample_runtime_tool("test_http", "http", true);
        row.execution_config = json!({
            "url_template": "https://example.com/api",
            "method": "GET"
        });

        let tool = RuntimeToolRegistry::materialise(&row);
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "test_http");
    }

    #[tokio::test]
    async fn materialise_script_tool() {
        let mut row = sample_runtime_tool("test_script", "script", true);
        row.execution_config = json!({
            "language": "javascript",
            "script": "return args;"
        });

        let tool = RuntimeToolRegistry::materialise(&row);
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "test_script");
    }

    #[tokio::test]
    async fn disabled_tools_are_skipped() {
        let row = sample_runtime_tool("disabled_tool", "http", false);
        let tool = RuntimeToolRegistry::materialise(&row);
        assert!(tool.is_none());
    }

    #[tokio::test]
    async fn invalid_tool_type_returns_none() {
        let row = sample_runtime_tool("weird_tool", "unknown", true);
        let tool = RuntimeToolRegistry::materialise(&row);
        assert!(tool.is_none());
    }

    #[tokio::test]
    async fn invalid_http_config_returns_none() {
        let mut row = sample_runtime_tool("bad_http", "http", true);
        row.execution_config = json!({"missing_required_url": true});
        let tool = RuntimeToolRegistry::materialise(&row);
        assert!(tool.is_none());
    }

    #[tokio::test]
    async fn invalid_script_config_returns_none() {
        let mut row = sample_runtime_tool("bad_script", "script", true);
        row.execution_config = json!({"language": "rust"}); // unsupported language
        let tool = RuntimeToolRegistry::materialise(&row);
        assert!(tool.is_none());
    }

    // -------------------------------------------------------------------------
    // Tenant filtering tests
    // -------------------------------------------------------------------------

    fn make_registry_with_tools() -> RuntimeToolRegistry {
        let mut tools = HashMap::new();
        let mut meta = HashMap::new();

        // Public tool
        tools.insert(
            "public".into(),
            Arc::new(MockTool {
                name: "public".into(),
            }) as Arc<dyn Tool>,
        );
        meta.insert(
            "public".into(),
            RuntimeTool {
                id: "1".into(),
                name: "public".into(),
                display_name: None,
                description: "Public".into(),
                tool_type: "http".into(),
                parameters_schema: json!({}),
                execution_config: json!({}),
                enabled: true,
                version: 1,
                is_public: true,
                created_by: None,
                tenant_id: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        // Private tool for tenant-a
        tools.insert(
            "private_a".into(),
            Arc::new(MockTool {
                name: "private_a".into(),
            }) as Arc<dyn Tool>,
        );
        meta.insert(
            "private_a".into(),
            RuntimeTool {
                id: "2".into(),
                name: "private_a".into(),
                display_name: None,
                description: "Private A".into(),
                tool_type: "http".into(),
                parameters_schema: json!({}),
                execution_config: json!({}),
                enabled: true,
                version: 1,
                is_public: false,
                created_by: None,
                tenant_id: Some("tenant-a".into()),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        // Disabled tool
        tools.insert(
            "disabled".into(),
            Arc::new(MockTool {
                name: "disabled".into(),
            }) as Arc<dyn Tool>,
        );
        meta.insert(
            "disabled".into(),
            RuntimeTool {
                id: "3".into(),
                name: "disabled".into(),
                display_name: None,
                description: "Disabled".into(),
                tool_type: "http".into(),
                parameters_schema: json!({}),
                execution_config: json!({}),
                enabled: false,
                version: 1,
                is_public: true,
                created_by: None,
                tenant_id: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );

        RuntimeToolRegistry {
            pool: sqlx::PgPool::connect_lazy("postgres://localhost/test")
                .expect("lazy pool never fails"),
            tools: Arc::new(ArcSwap::from_pointee(tools)),
            metadata: Arc::new(ArcSwap::from_pointee(meta)),
            reload_interval_secs: 0,
        }
    }

    #[tokio::test]
    async fn fleet_wide_has_tool() {
        let reg = make_registry_with_tools();
        assert!(reg.has_tool("public"));
        assert!(reg.has_tool("private_a"));
        assert!(reg.has_tool("disabled"));
        assert_eq!(reg.len(), 3);
    }

    #[tokio::test]
    async fn get_for_tenant_filters_correctly() {
        let reg = make_registry_with_tools();

        // tenant-a can see public + their own
        assert!(reg.get_for_tenant("public", Some("tenant-a")).is_some());
        assert!(reg.get_for_tenant("private_a", Some("tenant-a")).is_some());
        assert!(reg.get_for_tenant("disabled", Some("tenant-a")).is_none());

        // tenant-b can only see public
        assert!(reg.get_for_tenant("public", Some("tenant-b")).is_some());
        assert!(reg.get_for_tenant("private_a", Some("tenant-b")).is_none());
        assert!(reg.get_for_tenant("disabled", Some("tenant-b")).is_none());

        // No tenant filtering = fleet-wide
        assert!(reg.get_for_tenant("public", None).is_some());
        assert!(reg.get_for_tenant("private_a", None).is_some());
        assert!(reg.get_for_tenant("disabled", None).is_none());
    }

    #[tokio::test]
    async fn get_tool_definitions_for_tenant_filters() {
        let reg = make_registry_with_tools();

        let defs = reg.get_tool_definitions_for_tenant(Some("tenant-a"));
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"public"));
        assert!(names.contains(&"private_a"));
        assert!(!names.contains(&"disabled"));

        let defs = reg.get_tool_definitions_for_tenant(Some("tenant-b"));
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"public"));
        assert!(!names.contains(&"private_a"));
        assert!(!names.contains(&"disabled"));
    }

    #[tokio::test]
    async fn enabled_tool_names_filters_by_tenant() {
        let reg = make_registry_with_tools();

        let names = reg.enabled_tool_names(Some("tenant-a"));
        assert!(names.contains(&"public".to_string()));
        assert!(names.contains(&"private_a".to_string()));
        assert!(!names.contains(&"disabled".to_string()));

        let names = reg.enabled_tool_names(Some("tenant-b"));
        assert!(names.contains(&"public".to_string()));
        assert!(!names.contains(&"private_a".to_string()));
    }

    #[tokio::test]
    async fn get_tool_definitions_for_filters_names() {
        let reg = make_registry_with_tools();
        let defs = reg.get_tool_definitions_for(&["public", "disabled"]);
        let names: Vec<_> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"public"));
        assert!(!names.contains(&"disabled")); // disabled tools excluded
    }

    // -------------------------------------------------------------------------
    // Execution tests
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn execute_returns_tool_result() {
        let reg = make_registry_with_tools();
        let result = reg.execute("public", json!({"x": 1})).await.unwrap();
        assert_eq!(result["tool"], "public");
        assert_eq!(result["args"]["x"], 1);
    }

    #[tokio::test]
    async fn execute_not_found() {
        let reg = make_registry_with_tools();
        let err = reg.execute("ghost", json!({})).await.unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn execute_for_tenant_respects_access() {
        let reg = make_registry_with_tools();

        // tenant-a can execute private_a
        let result = reg
            .execute_for_tenant("private_a", json!({}), Some("tenant-a"))
            .await;
        assert!(result.is_ok());

        // tenant-b cannot
        let err = reg
            .execute_for_tenant("private_a", json!({}), Some("tenant-b"))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    // -------------------------------------------------------------------------
    // Hot-swap test
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn reload_swaps_atomically() {
        // Use a lazy pool so we never connect to a real DB.
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test")
            .expect("lazy pool never fails");
        let reg = Arc::new(RuntimeToolRegistry::with_interval(pool, 0));

        // Pre-populate with a mock
        let mut tools = HashMap::new();
        tools.insert(
            "old".into(),
            Arc::new(MockTool {
                name: "old".into(),
            }) as Arc<dyn Tool>,
        );
        reg.tools.store(Arc::new(tools));

        assert!(reg.has_tool("old"));
        assert!(!reg.has_tool("new"));

        // Simulate a hot swap
        let mut tools = HashMap::new();
        tools.insert(
            "new".into(),
            Arc::new(MockTool {
                name: "new".into(),
            }) as Arc<dyn Tool>,
        );
        reg.tools.store(Arc::new(tools));

        assert!(!reg.has_tool("old"));
        assert!(reg.has_tool("new"));
    }
}
