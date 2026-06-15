//! Runtime-defined tools database operations.
//!
//! Provides CRUD for `runtime_tools`, `runtime_tool_versions`, and
//! `runtime_tool_executions` tables (migration 015).
//!
//! # Design Notes
//!
//! - `RuntimeToolStore` holds a `&PgPool` (same lifetime pattern as
//!   `FleetProviderSecretsStore`).
//! - JSONB columns bind directly to `serde_json::Value` via sqlx.
//! - Timestamps read as `chrono::DateTime<chrono::Utc>`.
//! - All UUID columns are stored/transferred as `String` (matches the rest of
//!   the `ares-db` crate).
//! - `update` automatically snapshots the previous state into
//!   `runtime_tool_versions` before mutating the main row.

use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

// =============================================================================
// Structs
// =============================================================================

/// One persisted row in `runtime_tools`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeTool {
    pub id: String,
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    pub tool_type: String,
    pub parameters_schema: serde_json::Value,
    pub execution_config: serde_json::Value,
    pub enabled: bool,
    pub version: i32,
    pub is_public: bool,
    pub created_by: Option<String>,
    pub tenant_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// One persisted row in `runtime_tool_versions`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeToolVersion {
    pub id: String,
    pub tool_id: String,
    pub version: i32,
    pub parameters_schema: serde_json::Value,
    pub execution_config: serde_json::Value,
    pub description: Option<String>,
    pub changed_by: Option<String>,
    pub change_summary: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// One persisted row in `runtime_tool_executions`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeToolExecution {
    pub id: String,
    pub tool_id: String,
    pub tenant_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub input_args: serde_json::Value,
    pub output_result: Option<serde_json::Value>,
    pub status: String,
    pub error_message: Option<String>,
    pub duration_ms: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Request body for creating a runtime tool.
#[derive(Debug, Deserialize)]
pub struct CreateRuntimeToolRequest {
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    pub tool_type: String,
    pub parameters_schema: serde_json::Value,
    pub execution_config: serde_json::Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_false")]
    pub is_public: bool,
    pub created_by: Option<String>,
    pub tenant_id: Option<String>,
}

/// Request body for updating a runtime tool.
/// All fields are optional — missing fields retain their existing values.
#[derive(Debug, Deserialize, Default)]
pub struct UpdateRuntimeToolRequest {
    pub display_name: Option<Option<String>>,
    pub description: Option<String>,
    pub parameters_schema: Option<serde_json::Value>,
    pub execution_config: Option<serde_json::Value>,
    pub enabled: Option<bool>,
    pub is_public: Option<bool>,
    pub created_by: Option<Option<String>>,
    pub tenant_id: Option<Option<String>>,
}

fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}

// =============================================================================
// Store
// =============================================================================

/// CRUD for `runtime_tools`, `runtime_tool_versions`, and
/// `runtime_tool_executions`.
pub struct RuntimeToolStore<'a> {
    pool: &'a PgPool,
}

impl<'a> RuntimeToolStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    // -------------------------------------------------------------------------
    // RuntimeTool CRUD
    // -------------------------------------------------------------------------

    /// Return every tool ordered by name.
    pub async fn get_all(&self) -> Result<Vec<RuntimeTool>> {
        let rows = sqlx::query(
            "SELECT id::text AS id, name, display_name, description, tool_type, parameters_schema, \
                    execution_config, enabled, version, is_public, created_by, tenant_id, \
                    created_at, updated_at \
             FROM runtime_tools ORDER BY name",
        )
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        rows.iter().map(row_to_runtime_tool).collect()
    }

    /// Fetch a single tool by its UUID. Returns `None` if not found.
    pub async fn get_by_id(&self, id: &str) -> Result<Option<RuntimeTool>> {
        let row = sqlx::query(
            "SELECT id::text AS id, name, display_name, description, tool_type, parameters_schema, \
                    execution_config, enabled, version, is_public, created_by, tenant_id, \
                    created_at, updated_at \
             FROM runtime_tools WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        row.map(|r| row_to_runtime_tool(&r)).transpose()
    }

    /// Return tools visible to a tenant: tools owned by the tenant **plus**
    /// all public tools.
    pub async fn get_by_tenant(&self, tenant_id: &str) -> Result<Vec<RuntimeTool>> {
        let rows = sqlx::query(
            "SELECT id::text AS id, name, display_name, description, tool_type, parameters_schema, \
                    execution_config, enabled, version, is_public, created_by, tenant_id, \
                    created_at, updated_at \
             FROM runtime_tools \
             WHERE tenant_id = $1 OR is_public = true \
             ORDER BY name",
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        rows.iter().map(row_to_runtime_tool).collect()
    }

    /// Create a new runtime tool. Version starts at `1`.
    pub async fn create(&self, req: &CreateRuntimeToolRequest) -> Result<RuntimeTool> {
        if req.name.is_empty() {
            return Err(AppError::InvalidInput(
                "runtime tool name must not be empty".into(),
            ));
        }
        if req.description.is_empty() {
            return Err(AppError::InvalidInput(
                "runtime tool description must not be empty".into(),
            ));
        }
        validate_tool_type(&req.tool_type)?;
        if req.parameters_schema.is_null() {
            return Err(AppError::InvalidInput(
                "parameters_schema must not be null".into(),
            ));
        }
        if req.execution_config.is_null() {
            return Err(AppError::InvalidInput(
                "execution_config must not be null".into(),
            ));
        }
        validate_runtime_tool_scope(req.enabled, req.is_public, req.tenant_id.as_deref())?;

        let id = uuid::Uuid::new_v4().to_string();

        let row = sqlx::query(
            "INSERT INTO runtime_tools \
                (id, name, display_name, description, tool_type, parameters_schema, \
                 execution_config, enabled, version, is_public, created_by, tenant_id) \
             VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, $8, 1, $9, $10, $11) \
             RETURNING id::text AS id, name, display_name, description, tool_type, parameters_schema, \
                       execution_config, enabled, version, is_public, created_by, tenant_id, \
                       created_at, updated_at",
        )
        .bind(&id)
        .bind(&req.name)
        .bind(&req.display_name)
        .bind(&req.description)
        .bind(&req.tool_type)
        .bind(&req.parameters_schema)
        .bind(&req.execution_config)
        .bind(req.enabled)
        .bind(req.is_public)
        .bind(&req.created_by)
        .bind(&req.tenant_id)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_runtime_tool(&row)
    }

    /// Partial update of a runtime tool.
    ///
    /// This method automatically snapshots the **previous** state into
    /// `runtime_tool_versions` (with the old version number) before bumping
    /// the version on the main row.
    pub async fn update(&self, id: &str, req: &UpdateRuntimeToolRequest) -> Result<RuntimeTool> {
        validate_runtime_tool_update_scope_preflight(req)?;

        let existing = self
            .get_by_id(id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("runtime tool {id} not found")))?;

        let display_name = req
            .display_name
            .clone()
            .unwrap_or_else(|| existing.display_name.clone());
        let description = req
            .description
            .clone()
            .unwrap_or_else(|| existing.description.clone());
        let parameters_schema = req
            .parameters_schema
            .clone()
            .unwrap_or_else(|| existing.parameters_schema.clone());
        let execution_config = req
            .execution_config
            .clone()
            .unwrap_or_else(|| existing.execution_config.clone());
        let enabled = req.enabled.unwrap_or(existing.enabled);
        let is_public = req.is_public.unwrap_or(existing.is_public);
        let created_by = req
            .created_by
            .clone()
            .unwrap_or_else(|| existing.created_by.clone());
        let tenant_id = req
            .tenant_id
            .clone()
            .unwrap_or_else(|| existing.tenant_id.clone());
        validate_runtime_tool_scope(enabled, is_public, tenant_id.as_deref())?;
        let new_version = existing.version + 1;

        let mut tx = self.pool.begin().await.map_err(sqlx_err)?;

        // Snapshot current state into versions table.
        sqlx::query(
            "INSERT INTO runtime_tool_versions \
                (tool_id, version, parameters_schema, execution_config, description, change_summary) \
             VALUES ($1::uuid, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(existing.version)
        .bind(&existing.parameters_schema)
        .bind(&existing.execution_config)
        .bind(&existing.description)
        .bind("auto-snapshot on update")
        .execute(&mut *tx)
        .await
        .map_err(sqlx_err)?;

        // Update main row.
        let row = sqlx::query(
            "UPDATE runtime_tools SET \
                display_name = $1, description = $2, parameters_schema = $3, \
                execution_config = $4, enabled = $5, is_public = $6, created_by = $7, \
                tenant_id = $8, version = $9, updated_at = NOW() \
             WHERE id = $10::uuid \
             RETURNING id::text AS id, name, display_name, description, tool_type, parameters_schema, \
                       execution_config, enabled, version, is_public, created_by, tenant_id, \
                       created_at, updated_at",
        )
        .bind(&display_name)
        .bind(&description)
        .bind(&parameters_schema)
        .bind(&execution_config)
        .bind(enabled)
        .bind(is_public)
        .bind(&created_by)
        .bind(&tenant_id)
        .bind(new_version)
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(sqlx_err)?;

        tx.commit().await.map_err(sqlx_err)?;

        row_to_runtime_tool(&row)
    }

    /// Hard delete a tool (cascades to versions & executions).
    /// Returns the number of rows affected (`0` or `1`).
    pub async fn delete(&self, id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM runtime_tools WHERE id = $1::uuid")
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }

    // -------------------------------------------------------------------------
    // RuntimeToolVersion
    // -------------------------------------------------------------------------

    /// Return version history for a tool, most recent first.
    pub async fn get_versions(&self, tool_id: &str, limit: i64) -> Result<Vec<RuntimeToolVersion>> {
        let rows = sqlx::query(
            "SELECT id::text AS id, tool_id::text AS tool_id, version, parameters_schema, execution_config, description, \
                    changed_by, change_summary, created_at \
             FROM runtime_tool_versions \
             WHERE tool_id = $1::uuid \
             ORDER BY version DESC \
             LIMIT $2",
        )
        .bind(tool_id)
        .bind(limit)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;

        rows.iter().map(row_to_runtime_tool_version).collect()
    }

    /// Manually add a version snapshot. The caller is responsible for choosing a
    /// unique `version` number (the DB enforces uniqueness via
    /// `idx_runtime_tool_versions_unique`).
    #[allow(clippy::too_many_arguments)]
    pub async fn add_version(
        &self,
        tool_id: &str,
        version: i32,
        params: &serde_json::Value,
        exec: &serde_json::Value,
        description: Option<&str>,
        changed_by: Option<&str>,
        change_summary: Option<&str>,
    ) -> Result<RuntimeToolVersion> {
        let row = sqlx::query(
            "INSERT INTO runtime_tool_versions \
                (tool_id, version, parameters_schema, execution_config, description, \
                 changed_by, change_summary) \
             VALUES ($1::uuid, $2, $3, $4, $5, $6, $7) \
             RETURNING id::text AS id, tool_id::text AS tool_id, version, parameters_schema, execution_config, description, \
                       changed_by, change_summary, created_at",
        )
        .bind(tool_id)
        .bind(version)
        .bind(params)
        .bind(exec)
        .bind(description)
        .bind(changed_by)
        .bind(change_summary)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_runtime_tool_version(&row)
    }

    // -------------------------------------------------------------------------
    // RuntimeToolExecution
    // -------------------------------------------------------------------------

    /// Log a single tool execution.
    #[allow(clippy::too_many_arguments)]
    pub async fn log_execution(
        &self,
        tool_id: &str,
        tenant_id: Option<&str>,
        agent_run_id: Option<&str>,
        input_args: &serde_json::Value,
        output_result: Option<&serde_json::Value>,
        status: &str,
        error_message: Option<&str>,
        duration_ms: i64,
    ) -> Result<RuntimeToolExecution> {
        validate_status(status)?;

        let id = uuid::Uuid::new_v4().to_string();

        let row = sqlx::query(
            "INSERT INTO runtime_tool_executions \
                (id, tool_id, tenant_id, agent_run_id, input_args, output_result, \
                 status, error_message, duration_ms) \
             VALUES ($1::uuid, $2::uuid, $3, $4, $5, $6, $7, $8, $9) \
             RETURNING id::text AS id, tool_id::text AS tool_id, tenant_id, agent_run_id, input_args, output_result, \
                       status, error_message, duration_ms, created_at",
        )
        .bind(&id)
        .bind(tool_id)
        .bind(tenant_id)
        .bind(agent_run_id)
        .bind(input_args)
        .bind(output_result)
        .bind(status)
        .bind(error_message)
        .bind(duration_ms)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_runtime_tool_execution(&row)
    }

    /// Query execution logs with optional filters.
    ///
    /// All filter parameters are optional. Results ordered by `created_at DESC`.
    pub async fn get_executions(
        &self,
        tool_id: Option<&str>,
        tenant_id: Option<&str>,
        agent_run_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<RuntimeToolExecution>> {
        let mut sql = String::from(
            "SELECT id::text AS id, tool_id::text AS tool_id, tenant_id, agent_run_id, input_args, output_result, status, \
                    error_message, duration_ms, created_at \
             FROM runtime_tool_executions \
             WHERE 1=1",
        );
        let mut idx: i32 = 0;

        if tool_id.is_some() {
            idx += 1;
            sql.push_str(&format!(" AND tool_id = ${idx}::uuid"));
        }
        if tenant_id.is_some() {
            idx += 1;
            sql.push_str(&format!(" AND tenant_id = ${idx}"));
        }
        if agent_run_id.is_some() {
            idx += 1;
            sql.push_str(&format!(" AND agent_run_id = ${idx}"));
        }
        idx += 1;
        sql.push_str(&format!(" ORDER BY created_at DESC LIMIT ${idx}"));

        let mut query = sqlx::query(&sql);
        if let Some(v) = tool_id {
            query = query.bind(v);
        }
        if let Some(v) = tenant_id {
            query = query.bind(v);
        }
        if let Some(v) = agent_run_id {
            query = query.bind(v);
        }
        query = query.bind(limit);

        let rows = query.fetch_all(self.pool).await.map_err(sqlx_err)?;
        rows.iter().map(row_to_runtime_tool_execution).collect()
    }
}

// =============================================================================
// Row mappers
// =============================================================================

fn row_to_runtime_tool(row: &sqlx::postgres::PgRow) -> Result<RuntimeTool> {
    Ok(RuntimeTool {
        id: row.try_get("id").map_err(sqlx_err)?,
        name: row.try_get("name").map_err(sqlx_err)?,
        display_name: row.try_get("display_name").map_err(sqlx_err)?,
        description: row.try_get("description").map_err(sqlx_err)?,
        tool_type: row.try_get("tool_type").map_err(sqlx_err)?,
        parameters_schema: row.try_get("parameters_schema").map_err(sqlx_err)?,
        execution_config: row.try_get("execution_config").map_err(sqlx_err)?,
        enabled: row.try_get("enabled").map_err(sqlx_err)?,
        version: row.try_get("version").map_err(sqlx_err)?,
        is_public: row.try_get("is_public").map_err(sqlx_err)?,
        created_by: row.try_get("created_by").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
        updated_at: row.try_get("updated_at").map_err(sqlx_err)?,
    })
}

fn row_to_runtime_tool_version(row: &sqlx::postgres::PgRow) -> Result<RuntimeToolVersion> {
    Ok(RuntimeToolVersion {
        id: row.try_get("id").map_err(sqlx_err)?,
        tool_id: row.try_get("tool_id").map_err(sqlx_err)?,
        version: row.try_get("version").map_err(sqlx_err)?,
        parameters_schema: row.try_get("parameters_schema").map_err(sqlx_err)?,
        execution_config: row.try_get("execution_config").map_err(sqlx_err)?,
        description: row.try_get("description").map_err(sqlx_err)?,
        changed_by: row.try_get("changed_by").map_err(sqlx_err)?,
        change_summary: row.try_get("change_summary").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
    })
}

fn row_to_runtime_tool_execution(row: &sqlx::postgres::PgRow) -> Result<RuntimeToolExecution> {
    Ok(RuntimeToolExecution {
        id: row.try_get("id").map_err(sqlx_err)?,
        tool_id: row.try_get("tool_id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        agent_run_id: row.try_get("agent_run_id").map_err(sqlx_err)?,
        input_args: row.try_get("input_args").map_err(sqlx_err)?,
        output_result: row.try_get("output_result").map_err(sqlx_err)?,
        status: row.try_get("status").map_err(sqlx_err)?,
        error_message: row.try_get("error_message").map_err(sqlx_err)?,
        duration_ms: row.try_get("duration_ms").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
    })
}

// =============================================================================
// Helpers
// =============================================================================

fn sqlx_err(e: sqlx::Error) -> AppError {
    AppError::Database(e.to_string())
}

fn validate_tool_type(t: &str) -> Result<()> {
    match t {
        "http" | "mcp" | "script" | "sql" => Ok(()),
        _ => Err(AppError::InvalidInput(format!(
            "Invalid tool_type '{t}'. Must be one of: http, mcp, script, sql"
        ))),
    }
}

fn validate_status(s: &str) -> Result<()> {
    match s {
        "success" | "error" | "timeout" => Ok(()),
        _ => Err(AppError::InvalidInput(format!(
            "Invalid status '{s}'. Must be one of: success, error, timeout"
        ))),
    }
}

pub fn validate_runtime_tool_scope(
    enabled: bool,
    is_public: bool,
    tenant_id: Option<&str>,
) -> Result<()> {
    if enabled && !is_public && tenant_id.is_none_or(|id| id.trim().is_empty()) {
        return Err(AppError::InvalidInput(
            "enabled private runtime tools require tenant_id".into(),
        ));
    }
    Ok(())
}

pub fn validate_runtime_tool_update_scope_preflight(req: &UpdateRuntimeToolRequest) -> Result<()> {
    if req.enabled == Some(true) && req.is_public == Some(false) {
        validate_runtime_tool_scope(
            true,
            false,
            req.tenant_id
                .as_ref()
                .and_then(|tenant_id| tenant_id.as_deref()),
        )?;
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Unit tests (no DB required)
    // -------------------------------------------------------------------------

    #[test]
    fn runtime_tool_serde_roundtrip() {
        let original = RuntimeTool {
            id: "uuid-1".into(),
            name: "test-tool".into(),
            display_name: Some("Test Tool".into()),
            description: "A test tool".into(),
            tool_type: "http".into(),
            parameters_schema: serde_json::json!({"type": "object"}),
            execution_config: serde_json::json!({"url": "https://example.com"}),
            enabled: true,
            version: 3,
            is_public: false,
            created_by: Some("tenant-a".into()),
            tenant_id: Some("tenant-a".into()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: RuntimeTool = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn runtime_tool_version_serde_roundtrip() {
        let original = RuntimeToolVersion {
            id: "uuid-v1".into(),
            tool_id: "uuid-tool".into(),
            version: 2,
            parameters_schema: serde_json::json!({"type": "object"}),
            execution_config: serde_json::json!({"url": "https://example.com"}),
            description: Some("old desc".into()),
            changed_by: Some("admin".into()),
            change_summary: Some("fixed url".into()),
            created_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: RuntimeToolVersion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn runtime_tool_execution_serde_roundtrip() {
        let original = RuntimeToolExecution {
            id: "uuid-exec".into(),
            tool_id: "uuid-tool".into(),
            tenant_id: Some("tenant-a".into()),
            agent_run_id: Some("run-1".into()),
            input_args: serde_json::json!({"q": "hello"}),
            output_result: Some(serde_json::json!({"answer": "world"})),
            status: "success".into(),
            error_message: None,
            duration_ms: 42,
            created_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: RuntimeToolExecution = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn create_request_defaults() {
        let json = r#"{"name":"t","description":"d","tool_type":"http","parameters_schema":{},"execution_config":{}}"#;
        let req: CreateRuntimeToolRequest = serde_json::from_str(json).unwrap();
        assert!(req.enabled);
        assert!(!req.is_public);
    }

    #[test]
    fn update_request_partial() {
        let json = r#"{"enabled":false}"#;
        let req: UpdateRuntimeToolRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.enabled, Some(false));
        assert_eq!(req.description, None);
    }

    #[test]
    fn validate_tool_type_accepts_valid() {
        for t in ["http", "mcp", "script", "sql"] {
            assert!(validate_tool_type(t).is_ok());
        }
    }

    #[test]
    fn validate_tool_type_rejects_invalid() {
        let err = validate_tool_type("invalid").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains("invalid"));
    }

    #[test]
    fn validate_status_accepts_valid() {
        for s in ["success", "error", "timeout"] {
            assert!(validate_status(s).is_ok());
        }
    }

    #[test]
    fn validate_status_rejects_invalid() {
        let err = validate_status("pending").unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[test]
    fn validate_runtime_tool_scope_rejects_enabled_private_without_tenant() {
        for tenant_id in [None, Some(""), Some("   ")] {
            let err = validate_runtime_tool_scope(true, false, tenant_id).unwrap_err();
            assert!(matches!(err, AppError::InvalidInput(_)));
            assert!(err
                .to_string()
                .contains("enabled private runtime tools require tenant_id"));
        }
    }

    #[test]
    fn validate_runtime_tool_scope_allows_public_or_disabled_without_tenant() {
        assert!(validate_runtime_tool_scope(true, true, None).is_ok());
        assert!(validate_runtime_tool_scope(false, false, None).is_ok());
        assert!(validate_runtime_tool_scope(true, false, Some("tenant-a")).is_ok());
    }

    #[test]
    fn validate_runtime_tool_update_scope_preflight_rejects_explicit_unreachable_scope() {
        let req = UpdateRuntimeToolRequest {
            enabled: Some(true),
            is_public: Some(false),
            tenant_id: Some(Some("   ".to_string())),
            ..Default::default()
        };
        let err = validate_runtime_tool_update_scope_preflight(&req).unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    // -------------------------------------------------------------------------
    // Integration tests (require a live Postgres instance)
    // -------------------------------------------------------------------------

    fn test_db_url() -> String {
        if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
            return url;
        }
        if let Ok(url) = std::env::var("DATABASE_URL") {
            if url.contains("/ares") && !url.contains("ares_test") {
                return url.replace("/ares", "/ares_test");
            }
            return url;
        }
        "postgres://postgres:postgres@localhost:5432/ares_test".into()
    }

    async fn try_test_pool() -> Option<PgPool> {
        let db = crate::PostgresClient::new_remote(test_db_url(), String::new())
            .await
            .ok()?;
        sqlx::migrate!("../../migrations")
            .run(&db.pool)
            .await
            .ok()?;
        Some(db.pool)
    }

    #[tokio::test]
    async fn integration_crud_round_trip() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RuntimeToolStore::new(&pool);

        // Clean up any leftover rows from a previous aborted run.
        let _ = sqlx::query("DELETE FROM runtime_tools WHERE name LIKE 'integration-test-%'")
            .execute(&pool)
            .await;

        // Create
        let req = CreateRuntimeToolRequest {
            name: "integration-test-http".into(),
            display_name: Some("Test HTTP".into()),
            description: "An integration test tool".into(),
            tool_type: "http".into(),
            parameters_schema: serde_json::json!({"type": "object", "properties": {}}),
            execution_config: serde_json::json!({"method": "GET", "url": "https://example.com"}),
            enabled: true,
            is_public: false,
            created_by: Some("integration-tenant".into()),
            tenant_id: Some("integration-tenant".into()),
        };
        let tool = store.create(&req).await.expect("create tool");
        assert_eq!(tool.name, "integration-test-http");
        assert_eq!(tool.version, 1);
        assert!(tool.enabled);

        // Get by id
        let fetched = store.get_by_id(&tool.id).await.expect("get_by_id");
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().id, tool.id);

        // Get all (should contain our tool)
        let all = store.get_all().await.expect("get_all");
        assert!(all.iter().any(|t| t.id == tool.id));

        // Update
        let update = UpdateRuntimeToolRequest {
            description: Some("Updated description".into()),
            enabled: Some(false),
            ..Default::default()
        };
        let updated = store.update(&tool.id, &update).await.expect("update");
        assert_eq!(updated.description, "Updated description");
        assert!(!updated.enabled);
        assert_eq!(updated.version, 2);

        // Versions
        let versions = store
            .get_versions(&tool.id, 10)
            .await
            .expect("get_versions");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, 1);

        // Log execution
        let exec = store
            .log_execution(
                &tool.id,
                None,
                None,
                &serde_json::json!({"q": "hello"}),
                Some(&serde_json::json!({"r": "world"})),
                "success",
                None,
                15,
            )
            .await
            .expect("log_execution");
        assert_eq!(exec.tool_id, tool.id);
        assert_eq!(exec.status, "success");

        // Get executions
        let execs = store
            .get_executions(Some(&tool.id), None, None, 10)
            .await
            .expect("get_executions");
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].id, exec.id);

        // Delete
        let deleted = store.delete(&tool.id).await.expect("delete");
        assert_eq!(deleted, 1);
        assert!(store
            .get_by_id(&tool.id)
            .await
            .expect("get_by_id after delete")
            .is_none());
    }

    #[tokio::test]
    async fn integration_get_by_tenant_scoping() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RuntimeToolStore::new(&pool);

        let tenant_a = format!("tenant-{}", uuid::Uuid::new_v4());
        let tenant_b = format!("tenant-{}", uuid::Uuid::new_v4());

        // Clean up
        let _ = sqlx::query("DELETE FROM runtime_tools WHERE name LIKE 'scoping-%'")
            .execute(&pool)
            .await;

        // Create private tool for tenant_a
        let private = store
            .create(&CreateRuntimeToolRequest {
                name: "scoping-private".into(),
                display_name: None,
                description: "private".into(),
                tool_type: "http".into(),
                parameters_schema: serde_json::json!({}),
                execution_config: serde_json::json!({}),
                enabled: true,
                is_public: false,
                created_by: Some(tenant_a.clone()),
                tenant_id: Some(tenant_a.clone()),
            })
            .await
            .expect("create private");

        // Create public tool owned by tenant_b
        let public = store
            .create(&CreateRuntimeToolRequest {
                name: "scoping-public".into(),
                display_name: None,
                description: "public".into(),
                tool_type: "http".into(),
                parameters_schema: serde_json::json!({}),
                execution_config: serde_json::json!({}),
                enabled: true,
                is_public: true,
                created_by: Some(tenant_b.clone()),
                tenant_id: Some(tenant_b.clone()),
            })
            .await
            .expect("create public");

        // tenant_a should see both their own tool and the public tool
        let a_tools = store
            .get_by_tenant(&tenant_a)
            .await
            .expect("get_by_tenant a");
        assert!(a_tools.iter().any(|t| t.id == private.id));
        assert!(a_tools.iter().any(|t| t.id == public.id));

        // tenant_b should see both (their own public + no private from a since a is not b)
        // Actually tenant_b sees public (their own) but not private (owned by a)
        let b_tools = store
            .get_by_tenant(&tenant_b)
            .await
            .expect("get_by_tenant b");
        assert!(b_tools.iter().any(|t| t.id == public.id));
        assert!(!b_tools.iter().any(|t| t.id == private.id));

        // Cleanup
        let _ = store.delete(&private.id).await;
        let _ = store.delete(&public.id).await;
    }

    #[tokio::test]
    async fn integration_create_validates_tool_type() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RuntimeToolStore::new(&pool);

        let req = CreateRuntimeToolRequest {
            name: "bad-type".into(),
            display_name: None,
            description: "d".into(),
            tool_type: "websocket".into(),
            parameters_schema: serde_json::json!({}),
            execution_config: serde_json::json!({}),
            enabled: true,
            is_public: false,
            created_by: None,
            tenant_id: None,
        };
        let err = store.create(&req).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }

    #[tokio::test]
    async fn integration_log_execution_validates_status() {
        let Some(pool) = try_test_pool().await else {
            eprintln!("SKIP: no postgres");
            return;
        };
        let store = RuntimeToolStore::new(&pool);

        let err = store
            .log_execution(
                "00000000-0000-0000-0000-000000000000",
                None,
                None,
                &serde_json::json!({}),
                None,
                "pending",
                None,
                0,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
    }
}
