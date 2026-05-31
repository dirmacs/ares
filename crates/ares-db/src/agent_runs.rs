use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
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

#[derive(Debug, Clone, Serialize)]
pub struct AgentRunStats {
    pub total_runs: i64,
    pub success_count: i64,
    pub failed_count: i64,
    pub avg_duration_ms: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlatformStats {
    pub total_tenants: i64,
    pub total_agents: i64,
    pub total_runs_today: i64,
    pub total_tokens_today: i64,
    pub active_alerts: i64,
}

#[derive(Debug, Clone, Serialize)]
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
    let rows = if let Some(name) = agent_name {
        sqlx::query(
            "SELECT id, tenant_id, agent_name, user_id, workspace_id, session_id, status,
                    input_tokens, output_tokens, duration_ms, error, created_at,
                    COALESCE(model_name, 'unknown') AS model_name,
                    COALESCE(provider_name, 'unknown') AS provider_name,
                    COALESCE(is_streaming, false) AS is_streaming,
                    request_source, product,
                    agent_config_source, agent_config_version, eruka_binding_id,
                    COALESCE(eruka_context_hit, false) AS eruka_context_hit,
                    COALESCE(eruka_read_count, 0)::BIGINT AS eruka_read_count,
                    COALESCE(eruka_write_count, 0)::BIGINT AS eruka_write_count
             FROM agent_runs WHERE tenant_id = $1 AND agent_name = $2
             ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(tenant_id)
        .bind(name)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query(
            "SELECT id, tenant_id, agent_name, user_id, workspace_id, session_id, status,
                    input_tokens, output_tokens, duration_ms, error, created_at,
                    COALESCE(model_name, 'unknown') AS model_name,
                    COALESCE(provider_name, 'unknown') AS provider_name,
                    COALESCE(is_streaming, false) AS is_streaming,
                    request_source, product,
                    agent_config_source, agent_config_version, eruka_binding_id,
                    COALESCE(eruka_context_hit, false) AS eruka_context_hit,
                    COALESCE(eruka_read_count, 0)::BIGINT AS eruka_read_count,
                    COALESCE(eruka_write_count, 0)::BIGINT AS eruka_write_count
             FROM agent_runs WHERE tenant_id = $1
             ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(tenant_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    }
    .map_err(|e| AppError::Database(e.to_string()))?;

    rows.iter()
        .map(|row| {
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
        })
        .collect()
}

pub async fn get_agent_run_stats(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
) -> Result<AgentRunStats> {
    let row = sqlx::query(
        "SELECT
            COUNT(*) as total_runs,
            COUNT(*) FILTER (WHERE status = 'completed') as success_count,
            COUNT(*) FILTER (WHERE status = 'failed') as failed_count,
            COALESCE(AVG(duration_ms), 0)::BIGINT as avg_duration_ms,
            COALESCE(SUM(input_tokens), 0)::BIGINT as total_input_tokens,
            COALESCE(SUM(output_tokens), 0)::BIGINT as total_output_tokens
         FROM agent_runs WHERE tenant_id = $1 AND agent_name = $2",
    )
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
    let today_start = {
        let now = now_ts();
        now - (now % 86400)
    };

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
    use serde_json;

    // ── now_ts ──────────────────────────────────────────────────────

    #[test]
    fn now_ts_returns_positive_value() {
        let ts = now_ts();
        assert!(ts > 0, "now_ts should return a positive timestamp, got {ts}");
    }

    #[test]
    fn now_ts_is_reasonably_recent() {
        let ts = now_ts();
        // 2020-01-01 00:00:00 UTC = 1577836800
        assert!(ts >= 1_577_836_800, "timestamp should be after 2020, got {ts}");
    }

    #[test]
    fn now_ts_calls_are_non_decreasing() {
        let a = now_ts();
        let b = now_ts();
        assert!(b >= a, "consecutive calls should be non-decreasing");
    }

    // ── AgentRun serde roundtrip ────────────────────────────────────

    #[test]
    fn agent_run_serde_roundtrip() {
        let run = AgentRun {
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
        };

        let json = serde_json::to_string(&run).expect("serialize");
        let back: AgentRun = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.id, "run-001");
        assert_eq!(back.tenant_id, "t-abc");
        assert_eq!(back.agent_name, "coder");
        assert_eq!(back.user_id, Some("u-123".into()));
        assert_eq!(back.workspace_id, Some("ws-9".into()));
        assert_eq!(back.session_id, Some("sess-7".into()));
        assert_eq!(back.status, "completed");
        assert_eq!(back.input_tokens, 150);
        assert_eq!(back.output_tokens, 50);
        assert_eq!(back.duration_ms, 1200);
        assert_eq!(back.error, None);
        assert_eq!(back.created_at, 1_700_000_000);
        assert_eq!(back.model_name, "gpt-4o");
        assert_eq!(back.provider_name, "openai");
        assert!(back.is_streaming);
        assert_eq!(back.request_source, Some("api".into()));
        assert_eq!(back.product, Some("ares".into()));
        assert_eq!(back.agent_config_source, Some("db".into()));
        assert_eq!(back.agent_config_version, Some("v2".into()));
        assert_eq!(back.eruka_binding_id, Some("eb-1".into()));
        assert!(back.eruka_context_hit);
        assert_eq!(back.eruka_read_count, 3);
        assert_eq!(back.eruka_write_count, 1);
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

        let json = serde_json::to_string(&run).expect("serialize");
        let back: AgentRun = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.id, "run-002");
        assert_eq!(back.user_id, None);
        assert_eq!(back.error, Some("timeout".into()));
        assert!(!back.is_streaming);
        assert!(!back.eruka_context_hit);
    }

    // ── AgentRunMetadata default + serde ─────────────────────────────

    #[test]
    fn agent_run_metadata_default_values() {
        let meta = AgentRunMetadata::default();
        assert_eq!(meta.workspace_id, None);
        assert_eq!(meta.session_id, None);
        assert_eq!(meta.request_source, None);
        assert_eq!(meta.product, None);
        assert_eq!(meta.agent_config_source, None);
        assert_eq!(meta.agent_config_version, None);
        assert_eq!(meta.eruka_binding_id, None);
        assert!(!meta.eruka_context_hit);
        assert_eq!(meta.eruka_read_count, 0);
        assert_eq!(meta.eruka_write_count, 0);
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

        let json = serde_json::to_string(&meta).expect("serialize");
        let back: AgentRunMetadata = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.workspace_id, Some("ws-1".into()));
        assert_eq!(back.session_id, Some("sess-2".into()));
        assert_eq!(back.request_source, Some("cli".into()));
        assert_eq!(back.product, Some("eruka".into()));
        assert_eq!(back.agent_config_source, Some("file".into()));
        assert_eq!(back.agent_config_version, Some("v3".into()));
        assert_eq!(back.eruka_binding_id, Some("eb-4".into()));
        assert!(back.eruka_context_hit);
        assert_eq!(back.eruka_read_count, 10);
        assert_eq!(back.eruka_write_count, 5);
    }

    #[test]
    fn agent_run_metadata_default_serializes_correctly() {
        let meta = AgentRunMetadata::default();
        let json = serde_json::to_string(&meta).expect("serialize default");
        let back: AgentRunMetadata = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.eruka_context_hit, false);
        assert_eq!(back.eruka_read_count, 0);
        assert_eq!(back.eruka_write_count, 0);
    }

    // ── AgentRunStats serialization ──────────────────────────────────

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

        let json = serde_json::to_value(&stats).expect("serialize");
        assert_eq!(json["total_runs"], 100);
        assert_eq!(json["success_count"], 90);
        assert_eq!(json["failed_count"], 10);
        assert_eq!(json["avg_duration_ms"], 500);
        assert_eq!(json["total_input_tokens"], 50_000);
        assert_eq!(json["total_output_tokens"], 20_000);
    }

    // ── PlatformStats serialization ──────────────────────────────────

    #[test]
    fn platform_stats_serializes() {
        let stats = PlatformStats {
            total_tenants: 5,
            total_agents: 12,
            total_runs_today: 300,
            total_tokens_today: 75_000,
            active_alerts: 2,
        };

        let json = serde_json::to_value(&stats).expect("serialize");
        assert_eq!(json["total_tenants"], 5);
        assert_eq!(json["total_agents"], 12);
        assert_eq!(json["total_runs_today"], 300);
        assert_eq!(json["total_tokens_today"], 75_000);
        assert_eq!(json["active_alerts"], 2);
    }

    // ── AllAgentsEntry serialization ─────────────────────────────────

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

        let json = serde_json::to_value(&entry).expect("serialize");
        assert_eq!(json["tenant_id"], "t-1");
        assert_eq!(json["tenant_name"], "Acme");
        assert_eq!(json["agent_name"], "coder");
        assert_eq!(json["display_name"], "Code Agent");
        assert_eq!(json["model"], "gpt-4o");
        assert_eq!(json["enabled"], true);
        assert_eq!(json["total_runs"], 42);
        assert_eq!(json["last_run_at"], 1_700_000_000);
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

        let json = serde_json::to_value(&entry).expect("serialize");
        assert!(json["last_run_at"].is_null());
        assert_eq!(json["enabled"], false);
        assert_eq!(json["total_runs"], 0);
    }

}
