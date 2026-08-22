// ares/src/mcp/usage.rs
// Records every MCP tool call as a usage event.
// This feeds into the same usage/billing system as HTTP API calls.

use ares_types::types::AppError;
use chrono::{Datelike, Utc};
use uuid::Uuid;

#[cfg(feature = "mcp")]
use serde::{Deserialize, Serialize};

/// The type of MCP operation being tracked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "mcp", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "mcp", serde(rename_all = "snake_case"))]
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

    /// All variants — useful for exhaustive tests.
    pub const ALL: [Self; 8] = [
        Self::ListAgents,
        Self::RunAgent,
        Self::GetStatus,
        Self::DeployAgent,
        Self::GetUsage,
        Self::ErukaRead,
        Self::ErukaWrite,
        Self::ErukaSearch,
    ];
}

/// Billable tokens for a call: max(actual LLM tokens, operation minimum weight).
pub fn compute_effective_tokens(tokens_used: u64, operation: McpOperation) -> u64 {
    std::cmp::max(tokens_used, operation.token_weight())
}

/// Monthly token ceiling for a subscription tier.
pub fn tier_quota_limit(tier: &str) -> i64 {
    match tier {
        "free" => 10_000,
        "dev" => 500_000,
        "pro" => 5_000_000,
        "enterprise" => i64::MAX,
        _ => 10_000,
    }
}

/// Whether monthly usage is still below the tier ceiling (`used < limit`).
pub fn is_within_quota(used: i64, limit: i64) -> bool {
    used < limit
}

/// Sum of effective token counts across a batch of usage events.
pub fn aggregate_effective_tokens(events: &[(u64, McpOperation)]) -> i64 {
    events
        .iter()
        .map(|&(tokens_used, operation)| compute_effective_tokens(tokens_used, operation) as i64)
        .sum()
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
    let effective_tokens = compute_effective_tokens(tokens_used, operation);

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
    let max_tokens = tier_quota_limit(tier);

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
    let within_quota = is_within_quota(used, max_tokens);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_all_variants() {
        assert_eq!(McpOperation::ListAgents.as_str(), "mcp.ares_list_agents");
        assert_eq!(McpOperation::RunAgent.as_str(), "mcp.ares_run_agent");
        assert_eq!(McpOperation::GetStatus.as_str(), "mcp.ares_get_status");
        assert_eq!(McpOperation::DeployAgent.as_str(), "mcp.ares_deploy_agent");
        assert_eq!(McpOperation::GetUsage.as_str(), "mcp.ares_get_usage");
        assert_eq!(McpOperation::ErukaRead.as_str(), "mcp.eruka_read");
        assert_eq!(McpOperation::ErukaWrite.as_str(), "mcp.eruka_write");
        assert_eq!(McpOperation::ErukaSearch.as_str(), "mcp.eruka_search");
    }

    #[test]
    fn token_weight_all_variants() {
        assert_eq!(McpOperation::ListAgents.token_weight(), 1);
        assert_eq!(McpOperation::RunAgent.token_weight(), 10);
        assert_eq!(McpOperation::GetStatus.token_weight(), 1);
        assert_eq!(McpOperation::DeployAgent.token_weight(), 5);
        assert_eq!(McpOperation::GetUsage.token_weight(), 1);
        assert_eq!(McpOperation::ErukaRead.token_weight(), 1);
        assert_eq!(McpOperation::ErukaWrite.token_weight(), 2);
        assert_eq!(McpOperation::ErukaSearch.token_weight(), 2);
    }

    #[test]
    fn debug_clone_copy_traits() {
        let op = McpOperation::RunAgent;

        let debug_str = format!("{op:?}");
        assert_eq!(debug_str, "RunAgent");

        let copied = op;
        assert_eq!(copied.as_str(), op.as_str());

        let cloned = op.clone();
        assert_eq!(cloned.token_weight(), op.token_weight());
    }

    #[test]
    fn compute_effective_tokens_applies_minimum_weight() {
        assert_eq!(compute_effective_tokens(0, McpOperation::ListAgents), 1);
        assert_eq!(compute_effective_tokens(0, McpOperation::RunAgent), 10);
        assert_eq!(compute_effective_tokens(0, McpOperation::DeployAgent), 5);
        assert_eq!(compute_effective_tokens(0, McpOperation::ErukaWrite), 2);
    }

    #[test]
    fn compute_effective_tokens_uses_actual_when_higher() {
        assert_eq!(compute_effective_tokens(42, McpOperation::ListAgents), 42);
        assert_eq!(compute_effective_tokens(1_500, McpOperation::RunAgent), 1_500);
        assert_eq!(compute_effective_tokens(7, McpOperation::ErukaWrite), 7);
    }

    #[test]
    fn compute_effective_tokens_exact_weight_match() {
        assert_eq!(compute_effective_tokens(10, McpOperation::RunAgent), 10);
        assert_eq!(compute_effective_tokens(5, McpOperation::DeployAgent), 5);
    }

    #[test]
    fn aggregate_effective_tokens_empty() {
        assert_eq!(aggregate_effective_tokens(&[]), 0);
    }

    #[test]
    fn aggregate_effective_tokens_sums_billed_amounts() {
        let events = [
            (0, McpOperation::ListAgents),
            (0, McpOperation::RunAgent),
            (3, McpOperation::ErukaWrite),
            (100, McpOperation::GetStatus),
        ];
        // 1 + 10 + 3 + 100
        assert_eq!(aggregate_effective_tokens(&events), 114);
    }

    #[test]
    fn aggregate_effective_tokens_large_llm_run() {
        let events = [(50_000, McpOperation::RunAgent), (0, McpOperation::DeployAgent)];
        assert_eq!(aggregate_effective_tokens(&events), 50_005);
    }

    #[test]
    fn tier_quota_limit_known_tiers() {
        assert_eq!(tier_quota_limit("free"), 10_000);
        assert_eq!(tier_quota_limit("dev"), 500_000);
        assert_eq!(tier_quota_limit("pro"), 5_000_000);
        assert_eq!(tier_quota_limit("enterprise"), i64::MAX);
    }

    #[test]
    fn tier_quota_limit_unknown_defaults_to_free() {
        assert_eq!(tier_quota_limit("trial"), 10_000);
        assert_eq!(tier_quota_limit(""), 10_000);
    }

    #[test]
    fn is_within_quota_boundary() {
        assert!(is_within_quota(0, 10_000));
        assert!(is_within_quota(9_999, 10_000));
        assert!(!is_within_quota(10_000, 10_000));
        assert!(!is_within_quota(10_001, 10_000));
    }

    #[test]
    fn is_within_quota_enterprise_never_exhausted() {
        assert!(is_within_quota(i64::MAX - 1, i64::MAX));
        assert!(!is_within_quota(i64::MAX, i64::MAX));
    }

    #[test]
    fn quota_check_matches_tier_limit() {
        let limit = tier_quota_limit("free");
        assert!(is_within_quota(limit - 1, limit));
        assert!(!is_within_quota(limit, limit));
    }

    #[cfg(feature = "mcp")]
    mod serde_tests {
        use super::*;
        use serde_json::{json, Value};

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct UsageRecord {
            operation: McpOperation,
            tokens_used: u64,
            effective_tokens: u64,
            success: bool,
            duration_ms: u64,
        }

        #[test]
        fn mcp_operation_roundtrip_all_variants() {
            for op in McpOperation::ALL {
                let json = serde_json::to_string(&op).unwrap();
                let back: McpOperation = serde_json::from_str(&json).unwrap();
                assert_eq!(back, op);
            }
        }

        #[test]
        fn mcp_operation_serializes_snake_case() {
            assert_eq!(
                serde_json::to_value(McpOperation::RunAgent).unwrap(),
                json!("run_agent")
            );
            assert_eq!(
                serde_json::to_value(McpOperation::ErukaSearch).unwrap(),
                json!("eruka_search")
            );
        }

        #[test]
        fn mcp_operation_rejects_unknown_variant() {
            let err = serde_json::from_str::<McpOperation>(r#""not_a_tool""#).unwrap_err();
            assert!(err.is_data(), "expected unknown variant error");
        }

        #[test]
        fn usage_record_roundtrip_preserves_billed_tokens() {
            let record = UsageRecord {
                operation: McpOperation::RunAgent,
                tokens_used: 250,
                effective_tokens: compute_effective_tokens(250, McpOperation::RunAgent),
                success: true,
                duration_ms: 42,
            };
            assert_eq!(record.effective_tokens, 250);

            let json = serde_json::to_string(&record).unwrap();
            let back: UsageRecord = serde_json::from_str(&json).unwrap();
            assert_eq!(back, record);

            let value: Value = serde_json::from_str(&json).unwrap();
            assert_eq!(value["operation"], "run_agent");
            assert_eq!(value["tokens_used"], 250);
            assert_eq!(value["effective_tokens"], 250);
            assert_eq!(value["success"], true);
            assert_eq!(value["duration_ms"], 42);
        }

        #[test]
        fn usage_record_roundtrip_zero_tokens_non_llm() {
            let record = UsageRecord {
                operation: McpOperation::GetStatus,
                tokens_used: 0,
                effective_tokens: compute_effective_tokens(0, McpOperation::GetStatus),
                success: false,
                duration_ms: 0,
            };
            assert_eq!(record.effective_tokens, 1);

            let json = serde_json::to_string(&record).unwrap();
            let back: UsageRecord = serde_json::from_str(&json).unwrap();
            assert_eq!(back, record);
        }
    }

    #[cfg(feature = "postgres")]
    mod postgres_usage_tests {
        use super::*;
        use ares_store::PostgresClient;

        #[tokio::test]
        async fn record_mcp_usage_does_not_fail_when_database_unavailable() {
            let pool = PostgresClient::new_test().pool.clone();
            let result = record_mcp_usage(
                &pool,
                "tenant-test-001",
                McpOperation::ListAgents,
                0,
                true,
                25,
            )
            .await;

            assert!(result.is_ok(), "usage tracking must not block tool calls");
        }

        #[tokio::test]
        async fn check_quota_returns_error_when_database_unavailable() {
            let pool = PostgresClient::new_test().pool.clone();
            let result = check_quota(&pool, "tenant-test-001", "free").await;

            let err = result.expect_err("expected database error");
            match err {
                AppError::Database(msg) => {
                    assert!(
                        msg.contains("Failed to check quota"),
                        "unexpected message: {msg}"
                    );
                }
                other => panic!("expected Database error, got {other:?}"),
            }
        }
    }

}
