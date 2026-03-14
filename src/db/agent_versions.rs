//! Database operations for agent_config_versions table.
//!
//! Stores a snapshot of every agent TOON config on startup and on hot-reload,
//! enabling version history, auditing, and rollback (Sprint 12).
//!
//! Schema (migration 008):
//!   id, agent_id, version, config_json (JSONB), is_active, change_source, created_at

use anyhow::Result;
use sqlx::PgPool;
use tracing::{info, instrument, warn};

use crate::utils::toon_config::ToonAgentConfig;

/// Record a batch of agent configs into `agent_config_versions`.
/// Called on startup (change_source="startup") and on hot-reload (change_source="hot_reload").
///
/// Uses INSERT ... ON CONFLICT (agent_id, version) DO NOTHING so the same version
/// is never duplicated — only genuinely new or changed versions create rows.
#[instrument(skip(pool, agents), fields(count = agents.len()))]
pub async fn record_agent_versions(
    pool: &PgPool,
    agents: &[ToonAgentConfig],
    change_source: &str,
) -> Result<()> {
    let mut recorded = 0usize;

    for agent in agents {
        let config_json = serde_json::to_value(agent)
            .unwrap_or_else(|_| serde_json::json!({"name": agent.name}));

        match sqlx::query(
            "INSERT INTO agent_config_versions \
             (agent_id, version, config_json, is_active, change_source) \
             VALUES ($1, $2, $3, true, $4) \
             ON CONFLICT (agent_id, version) DO NOTHING",
        )
        .bind(&agent.name)
        .bind(&agent.version)
        .bind(&config_json)
        .bind(change_source)
        .execute(pool)
        .await
        {
            Ok(r) if r.rows_affected() > 0 => {
                recorded += 1;
            }
            Ok(_) => {
                // Already recorded — skip silently (same version)
            }
            Err(e) => {
                warn!(
                    agent = %agent.name,
                    version = %agent.version,
                    error = %e,
                    "Failed to record agent version"
                );
            }
        }
    }

    if recorded > 0 {
        info!(
            recorded,
            source = change_source,
            "Agent config versions recorded"
        );
    }

    Ok(())
}

/// Get the version history for a specific agent (most recent first)
pub async fn get_agent_version_history(
    pool: &PgPool,
    agent_id: &str,
    limit: i64,
) -> Result<Vec<AgentVersionRecord>> {
    let rows = sqlx::query_as!(
        AgentVersionRecord,
        r#"SELECT id, agent_id, version, config_json, is_active, change_source,
                  created_at as "created_at: chrono::DateTime<chrono::Utc>"
           FROM agent_config_versions
           WHERE agent_id = $1
           ORDER BY created_at DESC
           LIMIT $2"#,
        agent_id,
        limit,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// A row from agent_config_versions
#[derive(Debug, Clone)]
pub struct AgentVersionRecord {
    pub id: String,
    pub agent_id: String,
    pub version: String,
    pub config_json: serde_json::Value,
    pub is_active: bool,
    pub change_source: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}
