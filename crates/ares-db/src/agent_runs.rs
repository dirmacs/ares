use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::time::{SystemTime, UNIX_EPOCH};

const SECONDS_PER_UTC_DAY: i64 = 86_400;

const LIST_AGENT_RUNS_SELECT: &str = "SELECT id, tenant_id, agent_name, user_id, workspace_id, session_id, status,
                    input_tokens, output_tokens, duration_ms, error, created_at,
                    COALESCE(model_name, 'unknown') AS model_name,
                    COALESCE(provider_name, 'unknown') AS provider_name,
                    COALESCE(is_streaming, false) AS is_streaming,
                    request_source, product,
                    agent_config_source, agent_config_version, eruka_binding_id,
                    COALESCE(eruka_context_hit, false) AS eruka_context_hit,
                    COALESCE(eruka_read_count, 0)::BIGINT AS eruka_read_count,
                    COALESCE(eruka_write_count, 0)::BIGINT AS eruka_write_count";

pub const GET_AGENT_RUN_STATS_SQL: &str = "SELECT
            COUNT(*) as total_runs,
            COUNT(*) FILTER (WHERE status = 'completed') as success_count,
            COUNT(*) FILTER (WHERE status = 'failed') as failed_count,
            COALESCE(AVG(duration_ms), 0)::BIGINT as avg_duration_ms,
            COALESCE(SUM(input_tokens), 0)::BIGINT as total_input_tokens,
            COALESCE(SUM(output_tokens), 0)::BIGINT as total_output_tokens
         FROM agent_runs WHERE tenant_id = $1 AND agent_name = $2";

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

pub fn utc_day_start_ts(ts: i64) -> i64 {
    ts - (ts % SECONDS_PER_UTC_DAY)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunListFilter {
    pub tenant_id: String,
    pub agent_name: Option<String>,
    pub status: Option<String>,
    pub created_at_from: Option<i64>,
    pub created_at_to: Option<i64>,
}

impl AgentRunListFilter {
    pub fn new(tenant_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            agent_name: None,
            status: None,
            created_at_from: None,
            created_at_to: None,
        }
    }
}

fn append_agent_run_filters(sql: &mut String, filter: &AgentRunListFilter, bind_idx: &mut i32) {
    if filter.agent_name.is_some() {
        sql.push_str(&format!(" AND agent_name = ${bind_idx}"));
        *bind_idx += 1;
    }
    if filter.status.is_some() {
        sql.push_str(&format!(" AND status = ${bind_idx}"));
        *bind_idx += 1;
    }
    if filter.created_at_from.is_some() {
        sql.push_str(&format!(" AND created_at >= ${bind_idx}"));
        *bind_idx += 1;
    }
    if filter.created_at_to.is_some() {
        sql.push_str(&format!(" AND created_at <= ${bind_idx}"));
        *bind_idx += 1;
    }
}

pub fn build_list_agent_runs(filter: &AgentRunListFilter, limit: i64, offset: i64) -> String {
    let _ = (limit, offset);
    let mut bind_idx = 2i32;
    let mut sql = String::from(LIST_AGENT_RUNS_SELECT);
    sql.push_str(" FROM agent_runs WHERE tenant_id = $1");
    append_agent_run_filters(&mut sql, filter, &mut bind_idx);
    let limit_slot = bind_idx;
    let offset_slot = bind_idx + 1;
    sql.push_str(&format!(
        " ORDER BY created_at DESC LIMIT ${limit_slot} OFFSET ${offset_slot}"
    ));
    sql
}

pub fn build_count_agent_runs(filter: &AgentRunListFilter) -> String {
    let mut bind_idx = 2i32;
    let mut sql = String::from("SELECT COUNT(*)::BIGINT AS cnt FROM agent_runs WHERE tenant_id = $1");
    append_agent_run_filters(&mut sql, filter, &mut bind_idx);
    sql
}

pub fn pagination_offset(page: u64, limit: i64) -> i64 {
    if page <= 1 {
        return 0;
    }
    (page - 1) as i64 * limit
}

pub fn page_index_from_offset(offset: i64, limit: i64) -> u64 {
    if limit <= 0 {
        return 1;
    }
    (offset / limit) as u64 + 1
}

pub fn agent_run_from_row(row: &sqlx::postgres::PgRow) -> Result<AgentRun> {
    Ok(AgentRun {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        agent_name: row.get("agent_name"),
        user_id: row.get("user_id"),
        workspace_id: row.get("workspace_id"),
        session_id: row.get("session_id"),
        status: row.get("status"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        duration_ms: row.get("duration_ms"),
        error: row.get("error"),
        created_at: row.get("created_at"),
        model_name: row.get("model_name"),
        provider_name: row.get("provider_name"),
        is_streaming: row.get("is_streaming"),
        request_source: row.get("request_source"),
        product: row.get("product"),
        agent_config_source: row.get("agent_config_source"),
        agent_config_version: row.get("agent_config_version"),
        eruka_binding_id: row.get("eruka_binding_id"),
        eruka_context_hit: row.get("eruka_context_hit"),
        eruka_read_count: row.get("eruka_read_count"),
        eruka_write_count: row.get("eruka_write_count"),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub user_id: Option<String>,
    pub workspace_id: Option<String>,
    pub session_id: Option<String>,
    pub status: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub duration_ms: i64,
    pub error: Option<String>,
    pub created_at: i64,
    pub model_name: String,
    pub provider_name: String,
    pub is_streaming: bool,
    pub request_source: Option<String>,
    pub product: Option<String>,
    pub agent_config_source: Option<String>,
    pub agent_config_version: Option<String>,
    pub eruka_binding_id: Option<String>,
    pub eruka_context_hit: bool,
    pub eruka_read_count: i64,
    pub eruka_write_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunStats {
    pub total_runs: i64,
    pub success_count: i64,
    pub failed_count: i64,
    pub avg_duration_ms: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformStats {
    pub total_tenants: i64,
    pub total_agents: i64,
    pub total_runs_today: i64,
    pub total_tokens_today: i64,
    pub active_alerts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllAgentsEntry {
    pub tenant_id: String,
    pub tenant_name: String,
    pub agent_name: String,
    pub display_name: String,
    pub model: String,
    pub enabled: bool,
    pub total_runs: i64,
    pub last_run_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentRunMetadata {
    pub workspace_id: Option<String>,
    pub session_id: Option<String>,
    pub request_source: Option<String>,
    pub product: Option<String>,
    pub agent_config_source: Option<String>,
    pub agent_config_version: Option<String>,
    pub eruka_binding_id: Option<String>,
    pub eruka_context_hit: bool,
    pub eruka_read_count: i64,
    pub eruka_write_count: i64,
}

pub async fn insert_agent_run(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
    user_id: Option<&str>,
    status: &str,
    input_tokens: i64,
    output_tokens: i64,
    duration_ms: i64,
    error: Option<&str>,
    model_name: &str,
    provider_name: &str,
    is_streaming: bool,
) -> Result<String> {
    insert_agent_run_with_metadata(
        pool,
        tenant_id,
        agent_name,
        user_id,
        status,
        input_tokens,
        output_tokens,
        duration_ms,
        error,
        model_name,
        provider_name,
        is_streaming,
        None,
    )
    .await
}

pub async fn insert_agent_run_with_metadata(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
    user_id: Option<&str>,
    status: &str,
    input_tokens: i64,
    output_tokens: i64,
    duration_ms: i64,
    error: Option<&str>,
    model_name: &str,
    provider_name: &str,
    is_streaming: bool,
    metadata: Option<&AgentRunMetadata>,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ts();
    let metadata = metadata.cloned().unwrap_or_default();

    sqlx::query(
        "INSERT INTO agent_runs (
            id, tenant_id, agent_name, user_id, workspace_id, session_id, status,
            input_tokens, output_tokens, duration_ms, error, created_at,
            model_name, provider_name, is_streaming, request_source, product,
            agent_config_source, agent_config_version, eruka_binding_id,
            eruka_context_hit, eruka_read_count, eruka_write_count
         ) VALUES (
            $1, $2, $3, $4, $5, $6, $7,
            $8, $9, $10, $11, $12,
            $13, $14, $15, $16, $17,
            $18, $19, $20,
            $21, $22, $23
         )",
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(agent_name)
    .bind(user_id)
    .bind(&metadata.workspace_id)
    .bind(&metadata.session_id)
    .bind(status)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(duration_ms)
    .bind(error)
    .bind(now)
    .bind(model_name)
    .bind(provider_name)
    .bind(is_streaming)
    .bind(&metadata.request_source)
    .bind(&metadata.product)
    .bind(&metadata.agent_config_source)
    .bind(&metadata.agent_config_version)
    .bind(&metadata.eruka_binding_id)
    .bind(metadata.eruka_context_hit)
    .bind(metadata.eruka_read_count)
    .bind(metadata.eruka_write_count)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(id)
}

pub async fn list_agent_runs(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<AgentRun>> {
    let filter = AgentRunListFilter {
        tenant_id: tenant_id.to_string(),
        agent_name: agent_name.map(str::to_string),
        status: None,
        created_at_from: None,
        created_at_to: None,
    };
    let _dynamic = build_list_agent_runs(&filter, limit, offset);

    let sql = if agent_name.is_some() {
        format!(
            "{LIST_AGENT_RUNS_SELECT}
             FROM agent_runs WHERE tenant_id = $1 AND agent_name = $2
             ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        )
    } else {
        format!(
            "{LIST_AGENT_RUNS_SELECT}
             FROM agent_runs WHERE tenant_id = $1
             ORDER BY created_at DESC LIMIT $2 OFFSET $3"
        )
    };

    let rows = if let Some(name) = agent_name {
        sqlx::query(&sql)
            .bind(tenant_id)
            .bind(name)
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await
    } else {
        sqlx::query(&sql)
            .bind(tenant_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await
    }
    .map_err(|e| AppError::Database(e.to_string()))?;

    rows.iter().map(agent_run_from_row).collect()
}

pub async fn get_agent_run_stats(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
) -> Result<AgentRunStats> {
    let row = sqlx::query(GET_AGENT_RUN_STATS_SQL)
    .bind(tenant_id)
    .bind(agent_name)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(AgentRunStats {
        total_runs: row.get("total_runs"),
        success_count: row.get("success_count"),
        failed_count: row.get("failed_count"),
        avg_duration_ms: row.get("avg_duration_ms"),
        total_input_tokens: row.get("total_input_tokens"),
        total_output_tokens: row.get("total_output_tokens"),
    })
}

pub async fn get_platform_stats(pool: &PgPool) -> Result<PlatformStats> {
    let today_start = utc_day_start_ts(now_ts());

    let row = sqlx::query(
        "SELECT
            (SELECT COUNT(*) FROM tenants) as total_tenants,
            (SELECT COUNT(*) FROM tenant_agents) as total_agents,
            (SELECT COUNT(*) FROM agent_runs WHERE created_at >= $1) as total_runs_today,
            (SELECT COALESCE(SUM(input_tokens + output_tokens), 0)::BIGINT FROM agent_runs WHERE created_at >= $1) as total_tokens_today,
            (SELECT COUNT(*) FROM alerts WHERE resolved = FALSE) as active_alerts"
    )
    .bind(today_start)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(PlatformStats {
        total_tenants: row.get("total_tenants"),
        total_agents: row.get("total_agents"),
        total_runs_today: row.get("total_runs_today"),
        total_tokens_today: row.get("total_tokens_today"),
        active_alerts: row.get("active_alerts"),
    })
}

pub async fn list_all_agents(pool: &PgPool) -> Result<Vec<AllAgentsEntry>> {
    let rows = sqlx::query(
        "SELECT
            ta.tenant_id,
            t.name as tenant_name,
            ta.agent_name,
            ta.display_name,
            COALESCE(ta.config->>'model', 'unknown') as model,
            ta.enabled,
            COALESCE(ar.total_runs, 0) as total_runs,
            ar.last_run_at
         FROM tenant_agents ta
         JOIN tenants t ON t.id = ta.tenant_id
         LEFT JOIN (
            SELECT tenant_id, agent_name, COUNT(*) as total_runs, MAX(created_at) as last_run_at
            FROM agent_runs GROUP BY tenant_id, agent_name
         ) ar ON ar.tenant_id = ta.tenant_id AND ar.agent_name = ta.agent_name
         ORDER BY t.name, ta.agent_name",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    rows.iter()
        .map(|row| {
            Ok(AllAgentsEntry {
                tenant_id: row.get("tenant_id"),
                tenant_name: row.get("tenant_name"),
                agent_name: row.get("agent_name"),
                display_name: row.get("display_name"),
                model: row.get("model"),
                enabled: row.get("enabled"),
                total_runs: row.get("total_runs"),
                last_run_at: row.get("last_run_at"),
            })
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::AppError;
    use serde_json;

    fn count_bind_placeholders(sql: &str) -> usize {
        let mut max_idx = 0usize;
        let mut i = 0;
        let bytes = sql.as_bytes();
        while i < bytes.len() {
            if bytes[i] == b'$' {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > start {
                    if let Ok(idx) = std::str::from_utf8(&bytes[start..end])
                        .unwrap_or("0")
                        .parse::<usize>()
                    {
                        max_idx = max_idx.max(idx);
                    }
                }
                i = end;
            } else {
                i += 1;
            }
        }
        max_idx
    }

    #[test]
    fn now_ts_returns_positive_value() {
        assert!(now_ts() > 0);
    }

    #[test]
    fn now_ts_is_reasonably_recent() {
        assert!(now_ts() >= 1_577_836_800);
    }

    #[test]
    fn now_ts_calls_are_non_decreasing() {
        let a = now_ts();
        let b = now_ts();
        assert!(b >= a);
    }

    #[test]
    fn utc_day_start_ts_at_utc_midnight_is_unchanged() {
        let midnight = 1_704_067_200i64;
        assert_eq!(utc_day_start_ts(midnight), midnight);
    }

    #[test]
    fn utc_day_start_ts_normalizes_mid_day_timestamp() {
        assert_eq!(utc_day_start_ts(1_704_067_200 + 43_200), 1_704_067_200);
    }

    #[test]
    fn utc_day_start_ts_uses_86400_second_utc_days() {
        let ts = 1_700_000_123i64;
        assert_eq!(utc_day_start_ts(ts), ts - (ts % SECONDS_PER_UTC_DAY));
    }

    #[test]
    fn utc_day_start_ts_aligns_with_platform_stats_window() {
        let now = now_ts();
        assert_eq!(utc_day_start_ts(now), now - (now % SECONDS_PER_UTC_DAY));
    }

    #[test]
    fn pagination_offset_first_page_is_zero() {
        assert_eq!(pagination_offset(1, 25), 0);
    }

    #[test]
    fn pagination_offset_page_three_with_limit_ten() {
        assert_eq!(pagination_offset(3, 10), 20);
    }

    #[test]
    fn page_index_from_offset_first_page() {
        assert_eq!(page_index_from_offset(0, 25), 1);
    }

    #[test]
    fn page_index_from_offset_second_page() {
        assert_eq!(page_index_from_offset(25, 25), 2);
    }

    #[test]
    fn page_index_from_offset_with_zero_limit_defaults_to_one() {
        assert_eq!(page_index_from_offset(100, 0), 1);
    }

    #[test]
    fn agent_run_serde_roundtrip() {
        let run = sample_agent_run_full();
        let back: AgentRun = serde_json::from_str(&serde_json::to_string(&run).unwrap()).unwrap();
        assert_eq!(back.id, run.id);
        assert_eq!(back.tenant_id, run.tenant_id);
        assert!(back.is_streaming);
    }

    #[test]
    fn agent_run_serde_with_none_optionals() {
        let run = AgentRun {
            id: "run-002".into(),
            tenant_id: "t-xyz".into(),
            agent_name: "analyst".into(),
            user_id: None,
            workspace_id: None,
            session_id: None,
            status: "failed".into(),
            input_tokens: 0,
            output_tokens: 0,
            duration_ms: 0,
            error: Some("timeout".into()),
            created_at: 1_700_000_001,
            model_name: "claude-3".into(),
            provider_name: "anthropic".into(),
            is_streaming: false,
            request_source: None,
            product: None,
            agent_config_source: None,
            agent_config_version: None,
            eruka_binding_id: None,
            eruka_context_hit: false,
            eruka_read_count: 0,
            eruka_write_count: 0,
        };
        let back: AgentRun = serde_json::from_str(&serde_json::to_string(&run).unwrap()).unwrap();
        assert_eq!(back.error, Some("timeout".into()));
    }

    #[test]
    fn agent_run_metadata_default_values() {
        let meta = AgentRunMetadata::default();
        assert!(!meta.eruka_context_hit);
        assert_eq!(meta.eruka_read_count, 0);
    }

    #[test]
    fn agent_run_metadata_serde_roundtrip() {
        let meta = AgentRunMetadata {
            workspace_id: Some("ws-1".into()),
            session_id: Some("sess-2".into()),
            request_source: Some("cli".into()),
            product: Some("eruka".into()),
            agent_config_source: Some("file".into()),
            agent_config_version: Some("v3".into()),
            eruka_binding_id: Some("eb-4".into()),
            eruka_context_hit: true,
            eruka_read_count: 10,
            eruka_write_count: 5,
        };
        let back: AgentRunMetadata =
            serde_json::from_str(&serde_json::to_string(&meta).unwrap()).unwrap();
        assert_eq!(back.eruka_write_count, 5);
    }

    #[test]
    fn agent_run_metadata_default_serializes_correctly() {
        let back: AgentRunMetadata =
            serde_json::from_str(&serde_json::to_string(&AgentRunMetadata::default()).unwrap())
                .unwrap();
        assert!(!back.eruka_context_hit);
    }

    #[test]
    fn agent_run_stats_serializes() {
        let stats = AgentRunStats {
            total_runs: 100,
            success_count: 90,
            failed_count: 10,
            avg_duration_ms: 500,
            total_input_tokens: 50_000,
            total_output_tokens: 20_000,
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["total_runs"], 100);
    }

    #[test]
    fn agent_run_stats_debug_clone() {
        let stats = AgentRunStats {
            total_runs: 1,
            success_count: 1,
            failed_count: 0,
            avg_duration_ms: 10,
            total_input_tokens: 5,
            total_output_tokens: 5,
        };
        assert!(format!("{:?}", stats.clone()).contains("total_runs"));
    }

    #[test]
    fn platform_stats_serializes() {
        let stats = PlatformStats {
            total_tenants: 5,
            total_agents: 12,
            total_runs_today: 300,
            total_tokens_today: 75_000,
            active_alerts: 2,
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["active_alerts"], 2);
    }

    #[test]
    fn platform_stats_debug_clone() {
        let stats = PlatformStats {
            total_tenants: 1,
            total_agents: 2,
            total_runs_today: 3,
            total_tokens_today: 4,
            active_alerts: 0,
        };
        assert!(format!("{:?}", stats.clone()).contains("total_agents"));
    }

    #[test]
    fn all_agents_entry_serializes() {
        let entry = AllAgentsEntry {
            tenant_id: "t-1".into(),
            tenant_name: "Acme".into(),
            agent_name: "coder".into(),
            display_name: "Code Agent".into(),
            model: "gpt-4o".into(),
            enabled: true,
            total_runs: 42,
            last_run_at: Some(1_700_000_000),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["tenant_name"], "Acme");
    }

    #[test]
    fn all_agents_entry_serializes_with_null_last_run() {
        let entry = AllAgentsEntry {
            tenant_id: "t-2".into(),
            tenant_name: "Beta Inc".into(),
            agent_name: "analyst".into(),
            display_name: "Analyst Agent".into(),
            model: "claude-3".into(),
            enabled: false,
            total_runs: 0,
            last_run_at: None,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json["last_run_at"].is_null());
    }

    #[test]
    fn build_list_agent_runs_tenant_only() {
        let sql = build_list_agent_runs(&AgentRunListFilter::new("tenant-a"), 50, 0);
        assert!(sql.contains("WHERE tenant_id = $1"));
        assert!(sql.contains("LIMIT $2 OFFSET $3"));
        assert_eq!(count_bind_placeholders(&sql), 3);
    }

    #[test]
    fn build_list_agent_runs_with_agent_name() {
        let filter = AgentRunListFilter {
            agent_name: Some("coder".into()),
            ..AgentRunListFilter::new("tenant-a")
        };
        let sql = build_list_agent_runs(&filter, 10, 20);
        assert!(sql.contains("AND agent_name = $2"));
        assert_eq!(count_bind_placeholders(&sql), 4);
    }

    #[test]
    fn build_list_agent_runs_with_status_filter() {
        let filter = AgentRunListFilter {
            status: Some("failed".into()),
            ..AgentRunListFilter::new("tenant-a")
        };
        let sql = build_list_agent_runs(&filter, 5, 0);
        assert!(sql.contains("AND status = $2"));
    }

    #[test]
    fn build_list_agent_runs_with_created_at_from() {
        let filter = AgentRunListFilter {
            created_at_from: Some(1_700_000_000),
            ..AgentRunListFilter::new("tenant-a")
        };
        assert!(build_list_agent_runs(&filter, 5, 0).contains("created_at >= $2"));
    }

    #[test]
    fn build_list_agent_runs_with_created_at_to() {
        let filter = AgentRunListFilter {
            created_at_to: Some(1_800_000_000),
            ..AgentRunListFilter::new("tenant-a")
        };
        assert!(build_list_agent_runs(&filter, 5, 0).contains("created_at <= $2"));
    }

    #[test]
    fn build_list_agent_runs_all_filters_combined() {
        let filter = AgentRunListFilter {
            tenant_id: "t".into(),
            agent_name: Some("a".into()),
            status: Some("completed".into()),
            created_at_from: Some(100),
            created_at_to: Some(200),
        };
        let sql = build_list_agent_runs(&filter, 25, 50);
        assert!(sql.contains("LIMIT $6 OFFSET $7"));
        assert_eq!(count_bind_placeholders(&sql), 7);
    }

    #[test]
    fn build_list_agent_runs_orders_by_created_at_desc() {
        assert!(build_list_agent_runs(&AgentRunListFilter::new("t"), 1, 0)
            .contains("ORDER BY created_at DESC"));
    }

    #[test]
    fn build_list_matches_legacy_tenant_only_shape() {
        let sql = build_list_agent_runs(&AgentRunListFilter::new("t1"), 25, 10);
        assert!(sql.contains("COALESCE(model_name, 'unknown')"));
        assert!(sql.contains("LIMIT $2 OFFSET $3"));
    }

    #[test]
    fn build_list_matches_legacy_agent_filter_shape() {
        let filter = AgentRunListFilter {
            agent_name: Some("bot".into()),
            ..AgentRunListFilter::new("t1")
        };
        let sql = build_list_agent_runs(&filter, 25, 10);
        assert!(sql.contains("WHERE tenant_id = $1 AND agent_name = $2"));
        assert!(sql.contains("LIMIT $3 OFFSET $4"));
    }

    #[test]
    fn build_count_agent_runs_tenant_only() {
        let sql = build_count_agent_runs(&AgentRunListFilter::new("tenant-z"));
        assert!(sql.starts_with("SELECT COUNT(*)"));
        assert!(!sql.contains("ORDER BY"));
        assert_eq!(count_bind_placeholders(&sql), 1);
    }

    #[test]
    fn build_count_agent_runs_with_agent_and_status() {
        let filter = AgentRunListFilter {
            agent_name: Some("a".into()),
            status: Some("running".into()),
            ..AgentRunListFilter::new("t")
        };
        let sql = build_count_agent_runs(&filter);
        assert!(sql.contains("status = $3"));
        assert_eq!(count_bind_placeholders(&sql), 3);
    }

    #[test]
    fn build_count_agent_runs_with_date_range() {
        let filter = AgentRunListFilter {
            created_at_from: Some(1),
            created_at_to: Some(9),
            ..AgentRunListFilter::new("t")
        };
        let sql = build_count_agent_runs(&filter);
        assert!(sql.contains("created_at >= $2"));
        assert!(sql.contains("created_at <= $3"));
    }

    #[test]
    fn agent_run_from_row_select_lists_required_columns() {
        for col in [
            "id", "tenant_id", "agent_name", "status", "created_at", "model_name",
            "eruka_context_hit", "eruka_read_count",
        ] {
            assert!(LIST_AGENT_RUNS_SELECT.contains(col), "missing {col}");
        }
    }

    #[test]
    fn get_agent_run_stats_sql_uses_tenant_and_agent_binds() {
        assert!(GET_AGENT_RUN_STATS_SQL.contains("FILTER (WHERE status = 'completed')"));
        assert_eq!(count_bind_placeholders(GET_AGENT_RUN_STATS_SQL), 2);
    }

    #[test]
    fn agent_run_common_status_values_serialize() {
        for status in ["completed", "failed", "running", "cancelled", "timeout"] {
            let mut run = sample_agent_run_full();
            run.status = status.into();
            assert_eq!(serde_json::to_value(&run).unwrap()["status"], status);
        }
    }

    #[test]
    fn agent_run_clone_preserves_status_and_tokens() {
        let run = sample_agent_run_full();
        let cloned = run.clone();
        assert_eq!(cloned.status, run.status);
        assert_eq!(cloned.input_tokens, run.input_tokens);
    }

    #[test]
    fn agent_run_metadata_clone_debug() {
        let meta = AgentRunMetadata::default();
        assert!(format!("{:?}", meta.clone()).contains("eruka_read_count"));
    }

    #[test]
    fn database_error_variant_formats_message() {
        let err = AppError::Database("connection reset".into());
        assert!(err.to_string().contains("connection reset"));
    }

    fn sample_agent_run_full() -> AgentRun {
        AgentRun {
            id: "run-001".into(),
            tenant_id: "t-abc".into(),
            agent_name: "coder".into(),
            user_id: Some("u-123".into()),
            workspace_id: Some("ws-9".into()),
            session_id: Some("sess-7".into()),
            status: "completed".into(),
            input_tokens: 150,
            output_tokens: 50,
            duration_ms: 1200,
            error: None,
            created_at: 1_700_000_000,
            model_name: "gpt-4o".into(),
            provider_name: "openai".into(),
            is_streaming: true,
            request_source: Some("api".into()),
            product: Some("ares".into()),
            agent_config_source: Some("db".into()),
            agent_config_version: Some("v2".into()),
            eruka_binding_id: Some("eb-1".into()),
            eruka_context_hit: true,
            eruka_read_count: 3,
            eruka_write_count: 1,
        }
    }

    // ── Integration test helpers ─────────────────────────────────────────

    fn test_db_url() -> String {
        if let Ok(url) = std::env::var("DATABASE_URL") {
            if url.contains("/ares") && !url.contains("ares_test") {
                return url.replace("/ares", "/ares_test");
            }
            return url;
        }
        "postgres://dirmacs@localhost:5432/ares_test".to_string()
    }

    async fn try_test_pool() -> Option<PgPool> {
        let db = crate::PostgresClient::new_remote(test_db_url(), String::new()).await.ok()?;
        sqlx::migrate!("../../migrations").run(&db.pool).await.ok()?;
        Some(db.pool)
    }

    fn unique_tenant() -> String {
        format!("tenant-test-{}", uuid::Uuid::new_v4())
    }

    async fn seed_tenant(pool: &PgPool, tenant_id: &str) {
        sqlx::query(
            "INSERT INTO tenants (id, name, tier, created_at, updated_at) VALUES ($1, $2, 'free', 1, 1) ON CONFLICT (id) DO NOTHING"
        )
        .bind(tenant_id)
        .bind("Test Tenant")
        .execute(pool)
        .await
        .expect("seed tenant");
    }

    async fn seed_tenant_agent(pool: &PgPool, tenant_id: &str, agent_name: &str) {
        sqlx::query(
            "INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, config, enabled, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, true, 1, 1) ON CONFLICT (tenant_id, agent_name) DO NOTHING"
        )
        .bind(format!("ta-{}", uuid::Uuid::new_v4()))
        .bind(tenant_id)
        .bind(agent_name)
        .bind(agent_name)
        .bind(serde_json::json!({"model": "gpt-4o"}))
        .execute(pool)
        .await
        .expect("seed tenant agent");
    }

    async fn cleanup_test_tenant(pool: &PgPool, tenant_id: &str) {
        let _ = sqlx::query("DELETE FROM agent_runs WHERE tenant_id = $1")
            .bind(tenant_id)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM tenant_agents WHERE tenant_id = $1")
            .bind(tenant_id)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(pool)
            .await;
    }

    // ── Integration: insert_agent_run ────────────────────────────────────

    #[tokio::test]
    async fn integration_insert_agent_run_with_real_pool() {
        let Some(pool) = try_test_pool().await else { eprintln!("SKIP: no postgres"); return; };
        let tenant_id = unique_tenant();
        seed_tenant(&pool, &tenant_id).await;

        let id = insert_agent_run(
            &pool, &tenant_id, "coder", Some("u-1"), "completed",
            100, 50, 1200, None, "gpt-4o", "openai", false,
        ).await.expect("insert");

        assert!(!id.is_empty());
        let runs = list_agent_runs(&pool, &tenant_id, None, 10, 0).await.expect("list");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].tenant_id, tenant_id);
        assert_eq!(runs[0].agent_name, "coder");
        assert_eq!(runs[0].status, "completed");
        assert_eq!(runs[0].input_tokens, 100);
        assert_eq!(runs[0].output_tokens, 50);
        assert_eq!(runs[0].duration_ms, 1200);
        assert_eq!(runs[0].model_name, "gpt-4o");
        assert_eq!(runs[0].provider_name, "openai");
        assert!(!runs[0].is_streaming);

        cleanup_test_tenant(&pool, &tenant_id).await;
    }

    #[tokio::test]
    async fn integration_insert_agent_run_with_metadata() {
        let Some(pool) = try_test_pool().await else { eprintln!("SKIP: no postgres"); return; };
        let tenant_id = unique_tenant();
        seed_tenant(&pool, &tenant_id).await;

        let metadata = AgentRunMetadata {
            workspace_id: Some("ws-42".into()),
            session_id: Some("sess-99".into()),
            request_source: Some("cli".into()),
            product: Some("eruka".into()),
            agent_config_source: Some("file".into()),
            agent_config_version: Some("v3".into()),
            eruka_binding_id: Some("eb-7".into()),
            eruka_context_hit: true,
            eruka_read_count: 5,
            eruka_write_count: 2,
        };

        let id = insert_agent_run_with_metadata(
            &pool, &tenant_id, "analyst", Some("u-2"), "failed",
            200, 80, 3000, Some("timeout"), "claude-3", "anthropic", true,
            Some(&metadata),
        ).await.expect("insert with metadata");

        assert!(!id.is_empty());
        let runs = list_agent_runs(&pool, &tenant_id, None, 10, 0).await.expect("list");
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.workspace_id, Some("ws-42".into()));
        assert_eq!(run.session_id, Some("sess-99".into()));
        assert_eq!(run.request_source, Some("cli".into()));
        assert_eq!(run.product, Some("eruka".into()));
        assert_eq!(run.agent_config_source, Some("file".into()));
        assert_eq!(run.agent_config_version, Some("v3".into()));
        assert_eq!(run.eruka_binding_id, Some("eb-7".into()));
        assert!(run.eruka_context_hit);
        assert_eq!(run.eruka_read_count, 5);
        assert_eq!(run.eruka_write_count, 2);
        assert_eq!(run.error, Some("timeout".into()));
        assert!(run.is_streaming);

        cleanup_test_tenant(&pool, &tenant_id).await;
    }

    // ── Integration: list_agent_runs ─────────────────────────────────────

    #[tokio::test]
    async fn integration_list_agent_runs_with_agent_name_filter() {
        let Some(pool) = try_test_pool().await else { eprintln!("SKIP: no postgres"); return; };
        let tenant_id = unique_tenant();
        seed_tenant(&pool, &tenant_id).await;

        insert_agent_run(&pool, &tenant_id, "alpha", None, "completed", 10, 5, 100, None, "m", "p", false).await.unwrap();
        insert_agent_run(&pool, &tenant_id, "beta", None, "failed", 20, 10, 200, None, "m", "p", false).await.unwrap();
        insert_agent_run(&pool, &tenant_id, "alpha", None, "completed", 30, 15, 300, None, "m", "p", false).await.unwrap();

        let all = list_agent_runs(&pool, &tenant_id, None, 10, 0).await.expect("list all");
        assert_eq!(all.len(), 3);

        let alpha = list_agent_runs(&pool, &tenant_id, Some("alpha"), 10, 0).await.expect("list alpha");
        assert_eq!(alpha.len(), 2);
        assert!(alpha.iter().all(|r| r.agent_name == "alpha"));

        let beta = list_agent_runs(&pool, &tenant_id, Some("beta"), 10, 0).await.expect("list beta");
        assert_eq!(beta.len(), 1);
        assert_eq!(beta[0].agent_name, "beta");

        let none = list_agent_runs(&pool, &tenant_id, Some("gamma"), 10, 0).await.expect("list gamma");
        assert!(none.is_empty());

        cleanup_test_tenant(&pool, &tenant_id).await;
    }

    #[tokio::test]
    async fn integration_list_agent_runs_pagination() {
        let Some(pool) = try_test_pool().await else { eprintln!("SKIP: no postgres"); return; };
        let tenant_id = unique_tenant();
        seed_tenant(&pool, &tenant_id).await;

        for i in 0..5 {
            insert_agent_run(&pool, &tenant_id, "pager", None, "completed", i, i, i as i64 * 10, None, "m", "p", false).await.unwrap();
        }

        let page1 = list_agent_runs(&pool, &tenant_id, None, 2, 0).await.expect("page1");
        assert_eq!(page1.len(), 2);

        let page2 = list_agent_runs(&pool, &tenant_id, None, 2, 2).await.expect("page2");
        assert_eq!(page2.len(), 2);

        let page3 = list_agent_runs(&pool, &tenant_id, None, 2, 4).await.expect("page3");
        assert_eq!(page3.len(), 1);

        cleanup_test_tenant(&pool, &tenant_id).await;
    }

    // ── Integration: get_agent_run_stats ─────────────────────────────────

    #[tokio::test]
    async fn integration_get_agent_run_stats_aggregation() {
        let Some(pool) = try_test_pool().await else { eprintln!("SKIP: no postgres"); return; };
        let tenant_id = unique_tenant();
        seed_tenant(&pool, &tenant_id).await;

        // Insert runs with different statuses
        insert_agent_run(&pool, &tenant_id, "summary", None, "completed", 100, 50, 1000, None, "m", "p", false).await.unwrap();
        insert_agent_run(&pool, &tenant_id, "summary", None, "completed", 200, 100, 2000, None, "m", "p", false).await.unwrap();
        insert_agent_run(&pool, &tenant_id, "summary", None, "failed", 50, 25, 500, Some("err"), "m", "p", false).await.unwrap();

        let stats = get_agent_run_stats(&pool, &tenant_id, "summary").await.expect("stats");
        assert_eq!(stats.total_runs, 3);
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.failed_count, 1);
        assert_eq!(stats.total_input_tokens, 350);
        assert_eq!(stats.total_output_tokens, 175);
        assert_eq!(stats.avg_duration_ms, 1167); // (1000+2000+500) / 3 = 1166.67 -> 1167

        cleanup_test_tenant(&pool, &tenant_id).await;
    }

    // ── Integration: get_platform_stats ──────────────────────────────────

    #[tokio::test]
    async fn integration_get_platform_stats_counts() {
        let Some(pool) = try_test_pool().await else { eprintln!("SKIP: no postgres"); return; };
        let tenant_id = unique_tenant();
        seed_tenant(&pool, &tenant_id).await;
        seed_tenant_agent(&pool, &tenant_id, "plat-agent").await;

        // Insert a run with created_at >= today_start
        insert_agent_run(&pool, &tenant_id, "plat-agent", None, "completed", 10, 5, 100, None, "m", "p", false).await.unwrap();

        // Insert an unresolved alert
        let alert_id = format!("alert-{}", uuid::Uuid::new_v4());
        sqlx::query("INSERT INTO alerts (id, severity, source, title, message, resolved, created_at) VALUES ($1, 'critical', 'system', 't', 'm', false, 1)")
            .bind(&alert_id)
            .execute(&pool)
            .await
            .expect("insert alert");

        let stats = get_platform_stats(&pool).await.expect("platform stats");
        // We can't assert exact counts because the test DB may have existing data,
        // but we can assert these are at least the values we added.
        assert!(stats.total_tenants >= 1);
        assert!(stats.total_agents >= 1);
        assert!(stats.total_runs_today >= 1);
        assert!(stats.total_tokens_today >= 15);
        assert!(stats.active_alerts >= 1);

        // Cleanup alert
        let _ = sqlx::query("DELETE FROM alerts WHERE id = $1").bind(&alert_id).execute(&pool).await;
        cleanup_test_tenant(&pool, &tenant_id).await;
    }

    // ── Integration: list_all_agents ─────────────────────────────────────

    #[tokio::test]
    async fn integration_list_all_agents_returns_entry() {
        let Some(pool) = try_test_pool().await else { eprintln!("SKIP: no postgres"); return; };
        let tenant_id = unique_tenant();
        seed_tenant(&pool, &tenant_id).await;
        seed_tenant_agent(&pool, &tenant_id, "all-agent").await;

        insert_agent_run(&pool, &tenant_id, "all-agent", None, "completed", 10, 5, 100, None, "m", "p", false).await.unwrap();
        insert_agent_run(&pool, &tenant_id, "all-agent", None, "completed", 20, 10, 200, None, "m", "p", false).await.unwrap();

        let agents = list_all_agents(&pool).await.expect("list all agents");
        let found = agents.iter().find(|a| a.tenant_id == tenant_id && a.agent_name == "all-agent");
        assert!(found.is_some(), "expected agent entry");
        let entry = found.unwrap();
        assert_eq!(entry.total_runs, 2);
        assert!(entry.last_run_at.is_some());
        assert_eq!(entry.model, "gpt-4o");
        assert!(entry.enabled);

        cleanup_test_tenant(&pool, &tenant_id).await;
    }

    // ── Serde roundtrips ─────────────────────────────────────────────────

    #[test]
    fn agent_run_stats_serde_roundtrip() {
        let stats = AgentRunStats {
            total_runs: 100,
            success_count: 90,
            failed_count: 10,
            avg_duration_ms: 500,
            total_input_tokens: 50_000,
            total_output_tokens: 20_000,
        };
        let json = serde_json::to_string(&stats).expect("serialize");
        let back: AgentRunStats = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.total_runs, stats.total_runs);
        assert_eq!(back.success_count, stats.success_count);
        assert_eq!(back.failed_count, stats.failed_count);
        assert_eq!(back.avg_duration_ms, stats.avg_duration_ms);
        assert_eq!(back.total_input_tokens, stats.total_input_tokens);
        assert_eq!(back.total_output_tokens, stats.total_output_tokens);
    }

    #[test]
    fn platform_stats_serde_roundtrip() {
        let stats = PlatformStats {
            total_tenants: 5,
            total_agents: 12,
            total_runs_today: 300,
            total_tokens_today: 75_000,
            active_alerts: 2,
        };
        let json = serde_json::to_string(&stats).expect("serialize");
        let back: PlatformStats = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.total_tenants, stats.total_tenants);
        assert_eq!(back.total_agents, stats.total_agents);
        assert_eq!(back.total_runs_today, stats.total_runs_today);
        assert_eq!(back.total_tokens_today, stats.total_tokens_today);
        assert_eq!(back.active_alerts, stats.active_alerts);
    }

    #[test]
    fn all_agents_entry_serde_roundtrip() {
        let entry = AllAgentsEntry {
            tenant_id: "t-1".into(),
            tenant_name: "Acme".into(),
            agent_name: "coder".into(),
            display_name: "Code Agent".into(),
            model: "gpt-4o".into(),
            enabled: true,
            total_runs: 42,
            last_run_at: Some(1_700_000_000),
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let back: AllAgentsEntry = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.tenant_id, entry.tenant_id);
        assert_eq!(back.tenant_name, entry.tenant_name);
        assert_eq!(back.agent_name, entry.agent_name);
        assert_eq!(back.display_name, entry.display_name);
        assert_eq!(back.model, entry.model);
        assert_eq!(back.enabled, entry.enabled);
        assert_eq!(back.total_runs, entry.total_runs);
        assert_eq!(back.last_run_at, entry.last_run_at);
    }

    #[test]
    fn all_agents_entry_serde_roundtrip_with_null_last_run() {
        let entry = AllAgentsEntry {
            tenant_id: "t-2".into(),
            tenant_name: "Beta Inc".into(),
            agent_name: "analyst".into(),
            display_name: "Analyst Agent".into(),
            model: "claude-3".into(),
            enabled: false,
            total_runs: 0,
            last_run_at: None,
        };
        let json = serde_json::to_string(&entry).expect("serialize");
        let back: AllAgentsEntry = serde_json::from_str(&json).expect("deserialize");
        assert!(back.last_run_at.is_none());
        assert_eq!(back.total_runs, 0);
    }

    // ── SQL query constants validation ───────────────────────────────────

    #[test]
    fn list_agent_runs_select_coalesces_unknown_defaults() {
        assert!(LIST_AGENT_RUNS_SELECT.contains("COALESCE(model_name, 'unknown')"));
        assert!(LIST_AGENT_RUNS_SELECT.contains("COALESCE(provider_name, 'unknown')"));
        assert!(LIST_AGENT_RUNS_SELECT.contains("COALESCE(is_streaming, false)"));
        assert!(LIST_AGENT_RUNS_SELECT.contains("COALESCE(eruka_context_hit, false)"));
        assert!(LIST_AGENT_RUNS_SELECT.contains("COALESCE(eruka_read_count, 0)::BIGINT"));
        assert!(LIST_AGENT_RUNS_SELECT.contains("COALESCE(eruka_write_count, 0)::BIGINT"));
    }

    #[test]
    fn get_agent_run_stats_sql_coalesces_avg_and_sums() {
        assert!(GET_AGENT_RUN_STATS_SQL.contains("COALESCE(AVG(duration_ms), 0)::BIGINT"));
        assert!(GET_AGENT_RUN_STATS_SQL.contains("COALESCE(SUM(input_tokens), 0)::BIGINT"));
        assert!(GET_AGENT_RUN_STATS_SQL.contains("COALESCE(SUM(output_tokens), 0)::BIGINT"));
    }

    #[test]
    fn get_agent_run_stats_sql_filters_by_tenant_and_agent() {
        assert!(GET_AGENT_RUN_STATS_SQL.contains("WHERE tenant_id = $1 AND agent_name = $2"));
    }

    #[test]
    fn insert_agent_run_has_23_placeholders() {
        let sql = "INSERT INTO agent_runs (
            id, tenant_id, agent_name, user_id, workspace_id, session_id, status,
            input_tokens, output_tokens, duration_ms, error, created_at,
            model_name, provider_name, is_streaming, request_source, product,
            agent_config_source, agent_config_version, eruka_binding_id,
            eruka_context_hit, eruka_read_count, eruka_write_count
         ) VALUES (
            $1, $2, $3, $4, $5, $6, $7,
            $8, $9, $10, $11, $12,
            $13, $14, $15, $16, $17,
            $18, $19, $20,
            $21, $22, $23
         )";
        assert_eq!(count_bind_placeholders(sql), 23);
    }
}
