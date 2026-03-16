// ares/src/mcp/usage.rs
// Records every MCP tool call as a usage event.
// This feeds into the same usage/billing system as HTTP API calls.

use crate::types::AppError;
use chrono::{Datelike, Utc};
use uuid::Uuid;

/// The type of MCP operation being tracked.
#[derive(Debug, Clone, Copy)]
pub enum McpOperation {
    ListAgents,
    RunAgent,
    GetStatus,
    DeployAgent,
    GetUsage,
    ErukaRead,
    ErukaWrite,
    ErukaSearch,
}

impl McpOperation {
    /// Returns the operation name as stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ListAgents => "mcp.ares_list_agents",
            Self::RunAgent => "mcp.ares_run_agent",
            Self::GetStatus => "mcp.ares_get_status",
            Self::DeployAgent => "mcp.ares_deploy_agent",
            Self::GetUsage => "mcp.ares_get_usage",
            Self::ErukaRead => "mcp.eruka_read",
            Self::ErukaWrite => "mcp.eruka_write",
            Self::ErukaSearch => "mcp.eruka_search",
        }
    }

    /// Returns the token cost weight for this operation.
    /// Used for usage quota calculations.
    /// - Read operations: 1 unit
    /// - Write operations: 2 units
    /// - Agent runs: 10 units (LLM call involved)
    /// - Deploy: 5 units
    pub fn token_weight(&self) -> u64 {
        match self {
            Self::ListAgents => 1,
            Self::RunAgent => 10,
            Self::GetStatus => 1,
            Self::DeployAgent => 5,
            Self::GetUsage => 1,
            Self::ErukaRead => 1,
            Self::ErukaWrite => 2,
            Self::ErukaSearch => 2,
        }
    }
}

/// Records a single MCP usage event in the database.
///
/// # Arguments
/// - `pool`: PostgreSQL connection pool
/// - `tenant_id`: The tenant making the call
/// - `operation`: Which MCP tool was called
/// - `tokens_used`: Actual tokens consumed (0 for non-LLM calls, actual count for RunAgent)
/// - `success`: Whether the call succeeded
/// - `duration_ms`: How long the call took in milliseconds
///
/// # Errors
/// Returns error if the database insert fails. The caller should
/// log the error but NOT fail the tool call — usage tracking failure
/// should not block the user's request.
pub async fn record_mcp_usage(
    pool: &sqlx::PgPool,
    tenant_id: &str,
    operation: McpOperation,
    tokens_used: u64,
    success: bool,
    duration_ms: u64,
) -> Result<(), AppError> {
    let now_ts = Utc::now().timestamp();
    let op_name = operation.as_str();
    let weight = operation.token_weight();

    // The effective_tokens is the larger of actual tokens and the weight minimum.
    // This ensures that even non-LLM calls have a baseline cost.
    let effective_tokens = std::cmp::max(tokens_used, weight);

    // Insert into unified usage_events table (matches migrations/001_usage_events_unified.sql)
    let result = sqlx::query(
        r#"
        INSERT INTO usage_events (
            id, tenant_id, source, request_count, token_count,
            operation, tokens_used, effective_tokens, success, duration_ms, created_at
        )
        VALUES ($1, $2, 'mcp', 1, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(Uuid::new_v4().to_string())
    .bind(tenant_id)
    .bind(effective_tokens as i64) // token_count = effective_tokens for quota tracking
    .bind(op_name)
    .bind(tokens_used as i64)
    .bind(effective_tokens as i64)
    .bind(success)
    .bind(duration_ms as i64)
    .bind(now_ts)
    .execute(pool)
    .await;

    match result {
        Ok(_) => {
            tracing::debug!(
                tenant_id = tenant_id,
                operation = op_name,
                tokens = effective_tokens,
                success = success,
                duration_ms = duration_ms,
                "MCP usage event recorded"
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                tenant_id = tenant_id,
                operation = op_name,
                "Failed to record MCP usage event - continuing anyway"
            );
            // Don't fail the tool call - just log the error
            Ok(())
        }
    }
}

/// Checks if the tenant has exceeded their usage quota.
///
/// # Returns
/// - `Ok(true)` if the tenant is within their quota
/// - `Ok(false)` if the tenant has exceeded their quota
/// - `Err` if the database query fails
pub async fn check_quota(
    pool: &sqlx::PgPool,
    tenant_id: &str,
    tier: &str,
) -> Result<bool, AppError> {
    // Get the monthly quota for this tier
    let max_tokens: i64 = match tier {
        "free" => 10_000,
        "dev" => 500_000,
        "pro" => 5_000_000,
        "enterprise" => i64::MAX, // unlimited for enterprise
        _ => 10_000,              // default to free tier
    };

    // Sum effective_tokens for this month (created_at is a Unix BIGINT timestamp)
    let now = Utc::now();
    let start_of_month = now
        .date_naive()
        .with_day(1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();

    let row: (i64,) = sqlx::query_as(
        r#"
        SELECT COALESCE(SUM(effective_tokens)::bigint, 0)
        FROM usage_events
        WHERE tenant_id = $1 AND created_at >= $2
        "#,
    )
    .bind(tenant_id)
    .bind(start_of_month)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Database(format!("Failed to check quota: {}", e)))?;

    let used = row.0;
    let within_quota = used < max_tokens;

    if !within_quota {
        tracing::warn!(
            tenant_id = tenant_id,
            tier = tier,
            used = used,
            max = max_tokens,
            "Tenant exceeded MCP usage quota"
        );
    }

    Ok(within_quota)
}
