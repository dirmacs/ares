//! Runtime SQL tool executor for A.R.E.S.
//!
//! This tool executes SQL queries using templates configured via `execution_config`.
//! Queries use `$1, $2, ...` style positional parameters.  Arguments are bound in
//! **alphabetical key order** — e.g. `{"b": 1, "a": 2}` maps `$1 → 2`, `$2 → 1`.
//! Results are returned as a JSON array of objects.
//!
//! # Security
//!
//! The tool relies on PostgreSQL parameterised queries.  Never build SQL by
//! interpolating argument strings into the template; always use `$N` placeholders.

use crate::registry::Tool;
use ares_types::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Column, PgPool, Row};
use std::time::Duration;
use tokio::time::timeout;

// =============================================================================
// Configuration
// =============================================================================

/// SQL-specific configuration parsed from `execution_config` JSONB.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SqlToolConfig {
    /// SQL query template with `$1, $2, ...` positional placeholders.
    pub query_template: String,
    /// Optional PostgreSQL connection string.  If omitted the tool uses the
    /// default `PgPool` supplied at construction time.
    #[serde(default)]
    pub connection_string: Option<String>,
    /// Query timeout in seconds (default: 30).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Maximum number of result rows allowed (default: 1000).
    #[serde(default)]
    pub max_rows: Option<u64>,
}

impl Default for SqlToolConfig {
    fn default() -> Self {
        Self {
            query_template: String::new(),
            connection_string: None,
            timeout_secs: Some(30),
            max_rows: Some(1000),
        }
    }
}

// =============================================================================
// Tool implementation
// =============================================================================

/// Runtime SQL tool that executes parameterised PostgreSQL queries.
#[derive(Debug)]
pub struct SqlTool {
    name: String,
    description: String,
    parameters_schema: Value,
    config: SqlToolConfig,
    default_pool: Option<PgPool>,
    custom_pool: Option<PgPool>,
}

impl SqlTool {
    /// Create an SQL tool from its runtime configuration.
    ///
    /// `default_pool` is used when `config.connection_string` is `None`.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters_schema: Value,
        config: SqlToolConfig,
        default_pool: Option<PgPool>,
    ) -> Result<Self> {
        let custom_pool = config
            .connection_string
            .as_ref()
            .map(|url| {
                sqlx::postgres::PgPoolOptions::new()
                    .max_connections(1)
                    .connect_lazy(url)
                    .map_err(|e| {
                        ares_types::AppError::Configuration(format!(
                            "Invalid SQL tool connection string: {e}"
                        ))
                    })
            })
            .transpose()?;

        Ok(Self {
            name: name.into(),
            description: description.into(),
            parameters_schema,
            config,
            default_pool,
            custom_pool,
        })
    }

    /// Parse `execution_config` JSONB into [`SqlToolConfig`].
    pub fn parse_config(execution_config: &Value) -> Result<SqlToolConfig> {
        serde_json::from_value(execution_config.clone()).map_err(|e| {
            ares_types::AppError::Configuration(format!("Invalid SQL tool config: {e}"))
        })
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.timeout_secs.unwrap_or(30))
    }

    fn max_rows(&self) -> u64 {
        self.config.max_rows.unwrap_or(1000)
    }
}

#[async_trait]
impl Tool for SqlTool {
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
        let args_map = args.as_object().ok_or_else(|| {
            ares_types::AppError::InvalidInput("args must be a JSON object".to_string())
        })?;

        let pool = self
            .custom_pool
            .as_ref()
            .or(self.default_pool.as_ref())
            .ok_or_else(|| {
                ares_types::AppError::Configuration(
                    "No database pool available for SQL tool".to_string(),
                )
            })?;

        // Bind arguments in alphabetical key order so the mapping to `$1,$2,…`
        // is deterministic and self-documenting.
        let mut keys: Vec<&str> = args_map.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();

        let mut query = sqlx::query(&self.config.query_template);
        for key in &keys {
            let val = &args_map[*key];
            query = bind_json_value(query, val);
        }

        let fut = query.fetch_all(pool);
        let rows = timeout(self.timeout(), fut)
            .await
            .map_err(|_| ares_types::AppError::External("SQL query timed out".to_string()))?
            .map_err(|e| ares_types::AppError::Database(format!("SQL execution failed: {e}")))?;

        let max_rows = self.max_rows() as usize;
        if rows.len() > max_rows {
            return Err(ares_types::AppError::InvalidInput(format!(
                "Query returned {} rows, exceeding max_rows of {}",
                rows.len(),
                max_rows
            )));
        }

        let results: Result<Vec<Value>> = rows.iter().map(row_to_json).collect();

        Ok(json!(results?))
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Bind a `serde_json::Value` into a sqlx Postgres query.
fn bind_json_value<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    value: &'q Value,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match value {
        Value::Null => {
            query = query.bind(None::<String>);
        }
        Value::Bool(b) => {
            query = query.bind(*b);
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                query = query.bind(i);
            } else if let Some(f) = n.as_f64() {
                query = query.bind(f);
            } else {
                query = query.bind(n.to_string());
            }
        }
        Value::String(s) => {
            query = query.bind(s.as_str());
        }
        Value::Array(_) | Value::Object(_) => {
            query = query.bind(sqlx::types::Json(value));
        }
    }
    query
}

/// Convert a `PgRow` into a JSON object keyed by column names.
fn row_to_json(row: &sqlx::postgres::PgRow) -> Result<Value> {
    let mut obj = serde_json::Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        let name = col.name();
        let value = pg_value_to_json(row, i)?;
        obj.insert(name.to_string(), value);
    }
    Ok(Value::Object(obj))
}

/// Extract a single PostgreSQL cell value into `serde_json::Value`.
fn pg_value_to_json(row: &sqlx::postgres::PgRow, i: usize) -> Result<Value> {
    // JSON / JSONB columns
    if let Ok(sqlx::types::Json(v)) = row.try_get::<sqlx::types::Json<Value>, _>(i) {
        return Ok(v);
    }

    // Nullable string (covers TEXT, VARCHAR, CHAR, UUID, enums, … and NULL)
    if let Ok(opt) = row.try_get::<Option<String>, _>(i) {
        return Ok(opt.map_or(Value::Null, Value::String));
    }

    // Nullable integers (INT2/INT4 decode separately from INT8)
    if let Ok(opt) = row.try_get::<Option<i64>, _>(i) {
        return Ok(opt.map_or(Value::Null, |v| Value::Number(v.into())));
    }
    if let Ok(opt) = row.try_get::<Option<i32>, _>(i) {
        return Ok(opt.map_or(Value::Null, |v| Value::Number(v.into())));
    }
    if let Ok(opt) = row.try_get::<Option<i16>, _>(i) {
        return Ok(opt.map_or(Value::Null, |v| Value::Number(v.into())));
    }

    // Nullable float
    if let Ok(opt) = row.try_get::<Option<f64>, _>(i) {
        return Ok(opt.map_or(Value::Null, |v| {
            serde_json::Number::from_f64(v)
                .map_or_else(|| Value::String(v.to_string()), Value::Number)
        }));
    }

    // Nullable boolean
    if let Ok(opt) = row.try_get::<Option<bool>, _>(i) {
        return Ok(opt.map_or(Value::Null, Value::Bool));
    }

    // Fallback: try non-null string coercion for anything that can render as text.
    if let Ok(s) = row.try_get::<String, _>(i) {
        return Ok(Value::String(s));
    }

    Ok(Value::Null)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_test_tool(config: SqlToolConfig, pool: Option<PgPool>) -> SqlTool {
        SqlTool::new(
            "test_sql",
            "A test SQL tool",
            json!({
                "type": "object",
                "properties": {
                    "user_id": { "type": "string" }
                }
            }),
            config,
            pool,
        )
        .unwrap()
    }

    // -------------------------------------------------------------------------
    // Non-DB tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_name_and_description() {
        let tool = make_test_tool(SqlToolConfig::default(), None);
        assert_eq!(tool.name(), "test_sql");
        assert_eq!(tool.description(), "A test SQL tool");
    }

    #[test]
    fn test_parameters_schema() {
        let tool = make_test_tool(SqlToolConfig::default(), None);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn test_parse_config() {
        let raw = json!({
            "query_template": "SELECT * FROM users WHERE id = $1",
            "timeout_secs": 15,
            "max_rows": 50
        });
        let cfg = SqlTool::parse_config(&raw).unwrap();
        assert_eq!(cfg.query_template, "SELECT * FROM users WHERE id = $1");
        assert_eq!(cfg.timeout_secs, Some(15));
        assert_eq!(cfg.max_rows, Some(50));
        assert!(cfg.connection_string.is_none());
    }

    #[test]
    fn test_parse_config_with_connection_string() {
        let raw = json!({
            "query_template": "SELECT 1",
            "connection_string": "postgres://user:pass@localhost/db"
        });
        let cfg = SqlTool::parse_config(&raw).unwrap();
        assert_eq!(
            cfg.connection_string,
            Some("postgres://user:pass@localhost/db".to_string())
        );
    }

    #[test]
    fn test_parse_config_missing_optional_fields() {
        let raw = json!({
            "query_template": "SELECT 1"
        });
        let cfg = SqlTool::parse_config(&raw).unwrap();
        assert!(cfg.connection_string.is_none());
        assert_eq!(cfg.timeout_secs, None);
        assert_eq!(cfg.max_rows, None);
    }

    #[test]
    fn test_default_values() {
        let cfg = SqlToolConfig::default();
        assert!(cfg.query_template.is_empty());
        assert!(cfg.connection_string.is_none());
        assert_eq!(cfg.timeout_secs, Some(30));
        assert_eq!(cfg.max_rows, Some(1000));
    }

    #[test]
    fn test_invalid_connection_string() {
        let config = SqlToolConfig {
            query_template: "SELECT 1".to_string(),
            connection_string: Some("not-a-url".to_string()),
            ..Default::default()
        };
        let err = SqlTool::new("x", "y", json!({}), config, None).unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::Configuration(msg)
                if msg.contains("Invalid SQL tool connection string")
        ));
    }

    #[tokio::test]
    async fn test_non_object_args_rejected() {
        let tool = make_test_tool(SqlToolConfig::default(), None);
        let err = tool.execute(json!("not-an-object")).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg)
                if msg.contains("must be a JSON object")
        ));
    }

    #[tokio::test]
    async fn test_no_pool_available() {
        let config = SqlToolConfig {
            query_template: "SELECT 1".to_string(),
            ..Default::default()
        };
        let tool = make_test_tool(config, None);
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::Configuration(msg)
                if msg.contains("No database pool")
        ));
    }

    // -------------------------------------------------------------------------
    // DB-backed integration tests (skipped when TEST_DATABASE_URL is unset)
    // -------------------------------------------------------------------------

    async fn try_test_pool() -> Option<PgPool> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .ok()
    }

    #[tokio::test]
    async fn test_simple_select() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let config = SqlToolConfig {
            query_template: "SELECT $1::int as a, $2::text as b".to_string(),
            ..Default::default()
        };
        let tool = make_test_tool(config, Some(pool));
        // Alphabetic order => "a" = $1, "b" = $2
        let result = tool
            .execute(json!({ "a": 42, "b": "hello" }))
            .await
            .unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["a"], 42);
        assert_eq!(arr[0]["b"], "hello");
    }

    #[tokio::test]
    async fn test_mising_args_extra_ignored() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let config = SqlToolConfig {
            query_template: "SELECT $1::int as n".to_string(),
            ..Default::default()
        };
        let tool = make_test_tool(config, Some(pool));
        // "x" is ignored because there is no $2 in the query.
        let result = tool
            .execute(json!({ "n": 7, "x": "ignored" }))
            .await
            .unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["n"], 7);
    }

    #[tokio::test]
    async fn test_empty_result_set() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let config = SqlToolConfig {
            query_template: "SELECT * FROM (VALUES (1)) t WHERE false".to_string(),
            ..Default::default()
        };
        let tool = make_test_tool(config, Some(pool));
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result, json!([]));
    }

    #[tokio::test]
    async fn test_max_rows_enforced() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let config = SqlToolConfig {
            query_template: "SELECT generate_series(1, $1::int) as n".to_string(),
            max_rows: Some(5),
            ..Default::default()
        };
        let tool = make_test_tool(config, Some(pool));
        let err = tool.execute(json!({ "n": 10 })).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::InvalidInput(msg)
                if msg.contains("exceeding max_rows")
        ));
    }

    #[tokio::test]
    async fn test_max_rows_within_limit_ok() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let config = SqlToolConfig {
            query_template: "SELECT generate_series(1, $1::int) as n".to_string(),
            max_rows: Some(100),
            ..Default::default()
        };
        let tool = make_test_tool(config, Some(pool));
        let result = tool.execute(json!({ "n": 3 })).await.unwrap();
        assert_eq!(result.as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_timeout_enforced() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let config = SqlToolConfig {
            query_template: "SELECT pg_sleep($1::int) as result".to_string(),
            timeout_secs: Some(1),
            ..Default::default()
        };
        let tool = make_test_tool(config, Some(pool));
        let err = tool.execute(json!({ "seconds": 3 })).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::External(msg)
                if msg.contains("timed out")
        ));
    }

    #[tokio::test]
    async fn test_null_bool_number_params() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
        // Args bind alphabetically: flag→$1, name→$2, num→$3.
        let config = SqlToolConfig {
            query_template: "SELECT $1::boolean as flag, $2::text as name, $3::int as num"
                .to_string(),
            ..Default::default()
        };
        let tool = make_test_tool(config, Some(pool));
        let result = tool
            .execute(json!({ "flag": true, "name": null, "num": 99 }))
            .await
            .unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr[0]["flag"], true);
        assert_eq!(arr[0]["num"], 99);
        assert_eq!(arr[0]["name"], Value::Null);
    }

    #[tokio::test]
    async fn test_json_param_and_column() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let config = SqlToolConfig {
            query_template: "SELECT $1::jsonb as payload".to_string(),
            ..Default::default()
        };
        let tool = make_test_tool(config, Some(pool));
        let result = tool
            .execute(json!({ "payload": { "nested": [1, 2, 3] } }))
            .await
            .unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr[0]["payload"]["nested"][1], 2);
    }

    #[tokio::test]
    async fn test_multiple_rows() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let config = SqlToolConfig {
            query_template: "SELECT $1::int + generate_series(0, 2) as val".to_string(),
            ..Default::default()
        };
        let tool = make_test_tool(config, Some(pool));
        let result = tool.execute(json!({ "start": 10 })).await.unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["val"], 10);
        assert_eq!(arr[1]["val"], 11);
        assert_eq!(arr[2]["val"], 12);
    }

    #[tokio::test]
    async fn test_sql_error_propagated() {
        let Some(pool) = try_test_pool().await else {
            return;
        };
        let config = SqlToolConfig {
            query_template: "SELECT no_such_column FROM no_such_table".to_string(),
            ..Default::default()
        };
        let tool = make_test_tool(config, Some(pool));
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(
            err,
            ares_types::AppError::Database(msg)
                if msg.contains("SQL execution failed")
        ));
    }
}
