//! Agent schedules, event triggers, and pipeline links.
//!
//! Provides CRUD for `agent_schedules`, `event_triggers`, and `agent_pipelines`
//! tables (migration 020).

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

// =============================================================================
// Structs
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSchedule {
    pub id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub cron_expression: String,
    pub timezone: String,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventTrigger {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub event_type: String,
    pub event_config: serde_json::Value,
    pub target_agent: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentPipeline {
    pub id: String,
    pub tenant_id: String,
    pub source_agent: String,
    pub target_agent: String,
    pub condition: Option<String>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateScheduleRequest {
    pub tenant_id: String,
    pub agent_name: String,
    pub cron_expression: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateTriggerRequest {
    pub tenant_id: String,
    pub name: String,
    pub event_type: String,
    pub event_config: serde_json::Value,
    pub target_agent: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreatePipelineRequest {
    pub tenant_id: String,
    pub source_agent: String,
    pub target_agent: String,
    pub condition: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_timezone() -> String {
    "UTC".to_string()
}
fn default_true() -> bool {
    true
}

// =============================================================================
// Schedule Store
// =============================================================================

pub struct ScheduleStore<'a> {
    pool: &'a PgPool,
}

impl<'a> ScheduleStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_schedules(&self, tenant_id: &str) -> Result<Vec<AgentSchedule>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, agent_name, cron_expression, timezone, enabled, \
                    last_run_at, next_run_at, created_at, updated_at \
             FROM agent_schedules WHERE tenant_id = $1 ORDER BY agent_name",
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;
        rows.iter().map(row_to_schedule).collect()
    }

    pub async fn create_schedule(&self, req: &CreateScheduleRequest) -> Result<AgentSchedule> {
        if req.agent_name.is_empty() {
            return Err(AppError::InvalidInput(
                "agent_name must not be empty".into(),
            ));
        }
        if req.cron_expression.is_empty() {
            return Err(AppError::InvalidInput(
                "cron_expression must not be empty".into(),
            ));
        }
        let now = now_ts();
        let id = uuid::Uuid::new_v4().to_string();

        let row = sqlx::query(
            "INSERT INTO agent_schedules \
                (id, tenant_id, agent_name, cron_expression, timezone, enabled, \
                 last_run_at, next_run_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, NULL, NULL, $7, $7) \
             RETURNING id, tenant_id, agent_name, cron_expression, timezone, enabled, \
                       last_run_at, next_run_at, created_at, updated_at",
        )
        .bind(&id)
        .bind(&req.tenant_id)
        .bind(&req.agent_name)
        .bind(&req.cron_expression)
        .bind(&req.timezone)
        .bind(req.enabled)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_schedule(&row)
    }

    pub async fn delete_schedule(&self, id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM agent_schedules WHERE id = $1")
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }
}

// =============================================================================
// Event Trigger Store
// =============================================================================

pub struct EventTriggerStore<'a> {
    pool: &'a PgPool,
}

impl<'a> EventTriggerStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_triggers(&self, tenant_id: &str) -> Result<Vec<EventTrigger>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, name, event_type, event_config, target_agent, enabled, \
                    created_at, updated_at \
             FROM event_triggers WHERE tenant_id = $1 ORDER BY name",
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;
        rows.iter().map(row_to_trigger).collect()
    }

    pub async fn create_trigger(&self, req: &CreateTriggerRequest) -> Result<EventTrigger> {
        if req.name.is_empty() {
            return Err(AppError::InvalidInput("name must not be empty".into()));
        }
        if req.event_type.is_empty() {
            return Err(AppError::InvalidInput(
                "event_type must not be empty".into(),
            ));
        }
        validate_event_type(&req.event_type)?;
        let now = now_ts();
        let id = uuid::Uuid::new_v4().to_string();

        let row = sqlx::query(
            "INSERT INTO event_triggers \
                (id, tenant_id, name, event_type, event_config, target_agent, enabled, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8) \
             RETURNING id, tenant_id, name, event_type, event_config, target_agent, enabled, created_at, updated_at",
        )
        .bind(&id)
        .bind(&req.tenant_id)
        .bind(&req.name)
        .bind(&req.event_type)
        .bind(&req.event_config)
        .bind(&req.target_agent)
        .bind(req.enabled)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_trigger(&row)
    }

    pub async fn delete_trigger(&self, id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM event_triggers WHERE id = $1")
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }
}

// =============================================================================
// Pipeline Store
// =============================================================================

pub struct PipelineStore<'a> {
    pool: &'a PgPool,
}

impl<'a> PipelineStore<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_pipelines(&self, tenant_id: &str) -> Result<Vec<AgentPipeline>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, source_agent, target_agent, condition, enabled, \
                    created_at, updated_at \
             FROM agent_pipelines WHERE tenant_id = $1 ORDER BY source_agent, target_agent",
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;
        rows.iter().map(row_to_pipeline).collect()
    }

    pub async fn create_pipeline(&self, req: &CreatePipelineRequest) -> Result<AgentPipeline> {
        if req.source_agent.is_empty() {
            return Err(AppError::InvalidInput(
                "source_agent must not be empty".into(),
            ));
        }
        if req.target_agent.is_empty() {
            return Err(AppError::InvalidInput(
                "target_agent must not be empty".into(),
            ));
        }
        let now = now_ts();
        let id = uuid::Uuid::new_v4().to_string();

        let row = sqlx::query(
            "INSERT INTO agent_pipelines \
                (id, tenant_id, source_agent, target_agent, condition, enabled, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7) \
             RETURNING id, tenant_id, source_agent, target_agent, condition, enabled, created_at, updated_at",
        )
        .bind(&id)
        .bind(&req.tenant_id)
        .bind(&req.source_agent)
        .bind(&req.target_agent)
        .bind(&req.condition)
        .bind(req.enabled)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_pipeline(&row)
    }

    pub async fn delete_pipeline(&self, id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM agent_pipelines WHERE id = $1")
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }
}

// =============================================================================
// Row mappers
// =============================================================================

fn row_to_schedule(row: &sqlx::postgres::PgRow) -> Result<AgentSchedule> {
    Ok(AgentSchedule {
        id: row.try_get("id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        agent_name: row.try_get("agent_name").map_err(sqlx_err)?,
        cron_expression: row.try_get("cron_expression").map_err(sqlx_err)?,
        timezone: row.try_get("timezone").map_err(sqlx_err)?,
        enabled: row.try_get("enabled").map_err(sqlx_err)?,
        last_run_at: row.try_get("last_run_at").map_err(sqlx_err)?,
        next_run_at: row.try_get("next_run_at").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
        updated_at: row.try_get("updated_at").map_err(sqlx_err)?,
    })
}

fn row_to_trigger(row: &sqlx::postgres::PgRow) -> Result<EventTrigger> {
    Ok(EventTrigger {
        id: row.try_get("id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        name: row.try_get("name").map_err(sqlx_err)?,
        event_type: row.try_get("event_type").map_err(sqlx_err)?,
        event_config: row.try_get("event_config").map_err(sqlx_err)?,
        target_agent: row.try_get("target_agent").map_err(sqlx_err)?,
        enabled: row.try_get("enabled").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
        updated_at: row.try_get("updated_at").map_err(sqlx_err)?,
    })
}

fn row_to_pipeline(row: &sqlx::postgres::PgRow) -> Result<AgentPipeline> {
    Ok(AgentPipeline {
        id: row.try_get("id").map_err(sqlx_err)?,
        tenant_id: row.try_get("tenant_id").map_err(sqlx_err)?,
        source_agent: row.try_get("source_agent").map_err(sqlx_err)?,
        target_agent: row.try_get("target_agent").map_err(sqlx_err)?,
        condition: row.try_get("condition").map_err(sqlx_err)?,
        enabled: row.try_get("enabled").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
        updated_at: row.try_get("updated_at").map_err(sqlx_err)?,
    })
}

fn sqlx_err(e: sqlx::Error) -> AppError {
    AppError::Database(e.to_string())
}

fn validate_event_type(t: &str) -> Result<()> {
    const VALID: &[&str] = &["webhook", "document_upload", "field_change", "agent_complete"];
    if VALID.contains(&t) {
        Ok(())
    } else {
        Err(AppError::InvalidInput(format!(
            "invalid event_type: {t}. Must be one of: {}",
            VALID.join(", ")
        )))
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_ts_returns_positive_value() {
        assert!(now_ts() > 0);
    }

    #[test]
    fn validate_event_type_accepts_known() {
        assert!(validate_event_type("webhook").is_ok());
        assert!(validate_event_type("agent_complete").is_ok());
        assert!(validate_event_type("unknown").is_err());
    }
}
