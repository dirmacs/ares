use sqlx::{PgPool, Row};
use crate::types::{AppError, Result};
use serde::{Deserialize, Serialize};
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
    pub status: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub duration_ms: i64,
    pub error: Option<String>,
    pub created_at: i64,
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
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ts();

    sqlx::query(
        "INSERT INTO agent_runs (id, tenant_id, agent_name, user_id, status, input_tokens, output_tokens, duration_ms, error, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
    )
    .bind(&id)
    .bind(tenant_id)
    .bind(agent_name)
    .bind(user_id)
    .bind(status)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(duration_ms)
    .bind(error)
    .bind(now)
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
            "SELECT id, tenant_id, agent_name, user_id, status, input_tokens, output_tokens, duration_ms, error, created_at
             FROM agent_runs WHERE tenant_id = $1 AND agent_name = $2
             ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        )
        .bind(tenant_id)
        .bind(name)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query(
            "SELECT id, tenant_id, agent_name, user_id, status, input_tokens, output_tokens, duration_ms, error, created_at
             FROM agent_runs WHERE tenant_id = $1
             ORDER BY created_at DESC LIMIT $2 OFFSET $3"
        )
        .bind(tenant_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    }
    .map_err(|e| AppError::Database(e.to_string()))?;

    rows.iter().map(|row| {
        Ok(AgentRun {
            id: row.get("id"),
            tenant_id: row.get("tenant_id"),
            agent_name: row.get("agent_name"),
            user_id: row.get("user_id"),
            status: row.get("status"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            duration_ms: row.get("duration_ms"),
            error: row.get("error"),
            created_at: row.get("created_at"),
        })
    }).collect()
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
         FROM agent_runs WHERE tenant_id = $1 AND agent_name = $2"
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
         ORDER BY t.name, ta.agent_name"
    )
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    rows.iter().map(|row| {
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
    }).collect()
}
