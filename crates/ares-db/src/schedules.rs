//! Agent schedules, event triggers, and pipeline links.
//!
//! Provides CRUD for `agent_schedules`, `event_triggers`, and `agent_pipelines`
//! tables (migration 020).

use ares_types::types::{AppError, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::time::{SystemTime, UNIX_EPOCH};

/// Compute the next Unix timestamp at which a cron expression will fire.
///
/// Uses the `cron` crate with chrono and evaluates the expression in the
/// supplied IANA timezone before returning the UTC Unix timestamp.
pub fn compute_next_run(cron: &str, tz: &str) -> std::result::Result<i64, String> {
    compute_next_run_after(cron, tz, Utc::now())
}

fn compute_next_run_after(
    cron: &str,
    tz: &str,
    after_utc: DateTime<Utc>,
) -> std::result::Result<i64, String> {
    let timezone = parse_timezone(tz)?;
    let normalized_cron = normalize_cron_expression(cron);
    let schedule: Schedule = normalized_cron
        .as_ref()
        .parse()
        .map_err(|e| format!("Invalid cron expression '{}': {}", cron, e))?;
    let after_local = after_utc.with_timezone(&timezone);
    let next = schedule
        .after(&after_local)
        .next()
        .ok_or_else(|| "No future occurrence found for cron expression".to_string())?;
    Ok(next.timestamp())
}

fn normalize_cron_expression(cron: &str) -> std::borrow::Cow<'_, str> {
    let trimmed = cron.trim();
    if trimmed.starts_with('@') || trimmed.split_whitespace().count() != 5 {
        return std::borrow::Cow::Borrowed(cron);
    }
    std::borrow::Cow::Owned(format!("0 {trimmed}"))
}

fn parse_timezone(tz: &str) -> std::result::Result<Tz, String> {
    tz.parse::<Tz>()
        .map_err(|_| format!("Invalid timezone '{}': expected IANA timezone", tz))
}

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
    pub grace_period_seconds: i32,
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
    #[serde(default = "default_grace_period")]
    pub grace_period_seconds: i32,
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

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePipelineRequest {
    pub tenant_id: String,
    pub source_agent: String,
    pub target_agent: String,
    pub condition: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MissedRunAudit {
    pub id: String,
    pub schedule_id: String,
    pub expected_at: i64,
    pub detected_at: i64,
    pub action_taken: String,
    pub created_at: i64,
}

fn default_timezone() -> String {
    "UTC".to_string()
}
fn default_true() -> bool {
    true
}
fn default_grace_period() -> i32 {
    120
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
                    last_run_at, next_run_at, grace_period_seconds, created_at, updated_at \
             FROM agent_schedules WHERE tenant_id = $1 ORDER BY agent_name",
        )
        .bind(tenant_id)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;
        rows.iter().map(row_to_schedule).collect()
    }

    pub async fn create_schedule(&self, req: &CreateScheduleRequest) -> Result<AgentSchedule> {
        validate_schedule_request(req)?;
        let now = now_ts();
        let id = uuid::Uuid::new_v4().to_string();
        let next_run_at = compute_next_run(&req.cron_expression, &req.timezone)
            .map_err(|e| AppError::InvalidInput(e))?;

        let row = sqlx::query(
            "INSERT INTO agent_schedules \
                (id, tenant_id, agent_name, cron_expression, timezone, enabled, \
                 last_run_at, next_run_at, grace_period_seconds, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, NULL, $7, $8, $9, $9) \
             RETURNING id, tenant_id, agent_name, cron_expression, timezone, enabled, \
                       last_run_at, next_run_at, grace_period_seconds, created_at, updated_at",
        )
        .bind(&id)
        .bind(&req.tenant_id)
        .bind(&req.agent_name)
        .bind(&req.cron_expression)
        .bind(&req.timezone)
        .bind(req.enabled)
        .bind(next_run_at)
        .bind(req.grace_period_seconds)
        .bind(now)
        .fetch_one(self.pool)
        .await
        .map_err(sqlx_err)?;

        row_to_schedule(&row)
    }

    pub async fn update_schedule(
        &self,
        id: &str,
        req: &CreateScheduleRequest,
    ) -> Result<Option<AgentSchedule>> {
        validate_schedule_request(req)?;
        let now = now_ts();
        let next_run_at = compute_next_run(&req.cron_expression, &req.timezone)
            .map_err(|e| AppError::InvalidInput(e))?;

        let row = sqlx::query(&format!(
            "UPDATE agent_schedules SET \
                agent_name = $3, cron_expression = $4, timezone = $5, enabled = $6, \
                next_run_at = $7, grace_period_seconds = $8, updated_at = $9 \
             {} \
             RETURNING id, tenant_id, agent_name, cron_expression, timezone, enabled, \
                       last_run_at, next_run_at, grace_period_seconds, created_at, updated_at",
            update_schedule_where_clause_sql()
        ))
        .bind(id)
        .bind(&req.tenant_id)
        .bind(&req.agent_name)
        .bind(&req.cron_expression)
        .bind(&req.timezone)
        .bind(req.enabled)
        .bind(next_run_at)
        .bind(req.grace_period_seconds)
        .bind(now)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;

        row.map(|r| row_to_schedule(&r)).transpose()
    }

    pub async fn delete_schedule(&self, id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM agent_schedules WHERE id = $1")
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }

    pub async fn delete_schedule_for_tenant(&self, tenant_id: &str, id: &str) -> Result<u64> {
        let res = sqlx::query(delete_schedule_for_tenant_sql())
            .bind(id)
            .bind(tenant_id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// Return all enabled schedules whose `next_run_at` is in the past (or never set).
    pub async fn get_due_schedules(&self) -> Result<Vec<AgentSchedule>> {
        let now = now_ts();
        let rows = sqlx::query(
            "SELECT id, tenant_id, agent_name, cron_expression, timezone, enabled, \
                    last_run_at, next_run_at, grace_period_seconds, created_at, updated_at \
             FROM agent_schedules \
             WHERE enabled = TRUE AND (next_run_at IS NULL OR next_run_at <= $1)",
        )
        .bind(now)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;
        rows.iter().map(row_to_schedule).collect()
    }

    /// Update `last_run_at` and `next_run_at` after a scheduled run.
    pub async fn update_schedule_run(&self, id: &str, next_run_at: i64) -> Result<u64> {
        let now = now_ts();
        let res = sqlx::query(
            "UPDATE agent_schedules \
             SET last_run_at = $1, next_run_at = $2, updated_at = $1 \
             WHERE id = $3",
        )
        .bind(now)
        .bind(next_run_at)
        .bind(id)
        .execute(self.pool)
        .await
        .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }

    pub async fn insert_missed_run_audit(&self, audit: &MissedRunAudit) -> Result<bool> {
        let inserted = sqlx::query(insert_missed_run_audit_sql())
            .bind(&audit.id)
            .bind(&audit.schedule_id)
            .bind(audit.expected_at)
            .bind(audit.detected_at)
            .bind(&audit.action_taken)
            .bind(audit.created_at)
            .fetch_optional(self.pool)
            .await
            .map_err(sqlx_err)?
            .is_some();
        Ok(inserted)
    }

    pub async fn update_missed_run_action(
        &self,
        schedule_id: &str,
        expected_at: i64,
        action_taken: &str,
    ) -> Result<u64> {
        let result = sqlx::query(update_missed_run_action_sql())
            .bind(schedule_id)
            .bind(expected_at)
            .bind(action_taken)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(result.rows_affected())
    }

    pub async fn list_missed_runs(
        &self,
        schedule_id: &str,
        limit: i32,
    ) -> Result<Vec<MissedRunAudit>> {
        let rows = sqlx::query(
            "SELECT id, schedule_id, expected_at, detected_at, action_taken, created_at \
             FROM missed_runs \
             WHERE schedule_id = $1 \
             ORDER BY detected_at DESC \
             LIMIT $2",
        )
        .bind(schedule_id)
        .bind(limit)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;
        rows.iter().map(row_to_missed_run_audit).collect()
    }

    pub async fn list_missed_runs_for_tenant(
        &self,
        tenant_id: &str,
        schedule_id: &str,
        limit: i32,
    ) -> Result<Vec<MissedRunAudit>> {
        let rows = sqlx::query(list_missed_runs_for_tenant_sql())
            .bind(tenant_id)
            .bind(schedule_id)
            .bind(limit)
            .fetch_all(self.pool)
            .await
            .map_err(sqlx_err)?;
        rows.iter().map(row_to_missed_run_audit).collect()
    }

    /// Get enabled schedules whose `next_run_at` is older than the grace period.
    /// Used by the scheduler to detect missed runs.
    pub async fn get_overdue_for_catchup(&self) -> Result<Vec<AgentSchedule>> {
        let now = now_ts();
        let rows = sqlx::query(
            "SELECT id, tenant_id, agent_name, cron_expression, timezone, enabled, \
                    last_run_at, next_run_at, grace_period_seconds, created_at, updated_at \
             FROM agent_schedules \
             WHERE enabled = TRUE AND next_run_at IS NOT NULL AND \
                   next_run_at + grace_period_seconds < $1 AND \
                   NOT EXISTS ( \
                       SELECT 1 FROM missed_runs \
                       WHERE missed_runs.schedule_id = agent_schedules.id AND \
                             missed_runs.expected_at = agent_schedules.next_run_at \
                   ) \
             ORDER BY next_run_at ASC",
        )
        .bind(now)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;
        rows.iter().map(row_to_schedule).collect()
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

    pub async fn delete_trigger_for_tenant(&self, tenant_id: &str, id: &str) -> Result<u64> {
        let res = sqlx::query("DELETE FROM event_triggers WHERE tenant_id = $1 AND id = $2")
            .bind(tenant_id)
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// Return all triggers of a specific event type for a tenant.
    pub async fn list_by_event_type(
        &self,
        tenant_id: &str,
        event_type: &str,
    ) -> Result<Vec<EventTrigger>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, name, event_type, event_config, target_agent, enabled, \
                    created_at, updated_at \
             FROM event_triggers WHERE tenant_id = $1 AND event_type = $2 ORDER BY name",
        )
        .bind(tenant_id)
        .bind(event_type)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;
        rows.iter().map(row_to_trigger).collect()
    }

    /// Look up a single trigger by its id.
    pub async fn get_trigger(&self, id: &str) -> Result<Option<EventTrigger>> {
        let row = sqlx::query(
            "SELECT id, tenant_id, name, event_type, event_config, target_agent, enabled, \
                    created_at, updated_at \
             FROM event_triggers WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool)
        .await
        .map_err(sqlx_err)?;
        match row {
            Some(r) => Ok(Some(row_to_trigger(&r)?)),
            None => Ok(None),
        }
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
        validate_pipeline_request(req)?;
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

    pub async fn update_pipeline(
        &self,
        tenant_id: &str,
        id: &str,
        req: &CreatePipelineRequest,
    ) -> Result<Option<AgentPipeline>> {
        let mut req = req.clone();
        req.tenant_id = tenant_id.to_string();
        validate_pipeline_request(&req)?;
        let now = now_ts();
        let row = sqlx::query(update_pipeline_sql())
            .bind(tenant_id)
            .bind(id)
            .bind(&req.source_agent)
            .bind(&req.target_agent)
            .bind(&req.condition)
            .bind(req.enabled)
            .bind(now)
            .fetch_optional(self.pool)
            .await
            .map_err(sqlx_err)?;

        row.map(|row| row_to_pipeline(&row)).transpose()
    }

    pub async fn delete_pipeline_for_tenant(&self, tenant_id: &str, id: &str) -> Result<u64> {
        let res = sqlx::query(delete_pipeline_for_tenant_sql())
            .bind(tenant_id)
            .bind(id)
            .execute(self.pool)
            .await
            .map_err(sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// Return all enabled pipelines for a given source agent within a tenant.
    pub async fn get_pipelines_for_source(
        &self,
        tenant_id: &str,
        source_agent: &str,
    ) -> Result<Vec<AgentPipeline>> {
        let rows = sqlx::query(
            "SELECT id, tenant_id, source_agent, target_agent, condition, enabled, \
                    created_at, updated_at \
             FROM agent_pipelines \
             WHERE tenant_id = $1 AND source_agent = $2 AND enabled = TRUE \
             ORDER BY target_agent",
        )
        .bind(tenant_id)
        .bind(source_agent)
        .fetch_all(self.pool)
        .await
        .map_err(sqlx_err)?;
        rows.iter().map(row_to_pipeline).collect()
    }
}

// =============================================================================
// Row mappers
// =============================================================================

fn insert_missed_run_audit_sql() -> &'static str {
    "INSERT INTO missed_runs \
        (id, schedule_id, expected_at, detected_at, action_taken, created_at) \
     VALUES ($1, $2, $3, $4, $5, $6) \
     ON CONFLICT (schedule_id, expected_at) DO NOTHING \
     RETURNING id"
}

fn update_missed_run_action_sql() -> &'static str {
    "UPDATE missed_runs SET action_taken = $3 WHERE schedule_id = $1 AND expected_at = $2"
}

fn delete_pipeline_for_tenant_sql() -> &'static str {
    "DELETE FROM agent_pipelines WHERE tenant_id = $1 AND id = $2"
}

fn update_schedule_where_clause_sql() -> &'static str {
    "WHERE id = $1 AND tenant_id = $2"
}

fn delete_schedule_for_tenant_sql() -> &'static str {
    "DELETE FROM agent_schedules WHERE id = $1 AND tenant_id = $2"
}

fn list_missed_runs_for_tenant_sql() -> &'static str {
    "SELECT missed_runs.id, missed_runs.schedule_id, missed_runs.expected_at, \
            missed_runs.detected_at, missed_runs.action_taken, missed_runs.created_at \
     FROM missed_runs \
     JOIN agent_schedules ON agent_schedules.id = missed_runs.schedule_id \
     WHERE agent_schedules.tenant_id = $1 AND missed_runs.schedule_id = $2 \
     ORDER BY missed_runs.detected_at DESC \
     LIMIT $3"
}

fn validate_pipeline_request(req: &CreatePipelineRequest) -> Result<()> {
    if req.tenant_id.is_empty() {
        return Err(AppError::InvalidInput("tenant_id must not be empty".into()));
    }
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
    Ok(())
}

fn update_pipeline_sql() -> &'static str {
    "UPDATE agent_pipelines SET \
        source_agent = $3, target_agent = $4, condition = $5, enabled = $6, updated_at = $7 \
     WHERE tenant_id = $1 AND id = $2 \
     RETURNING id, tenant_id, source_agent, target_agent, condition, enabled, created_at, updated_at"
}

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
        grace_period_seconds: row.try_get("grace_period_seconds").map_err(sqlx_err)?,
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

fn row_to_missed_run_audit(row: &sqlx::postgres::PgRow) -> Result<MissedRunAudit> {
    Ok(MissedRunAudit {
        id: row.try_get("id").map_err(sqlx_err)?,
        schedule_id: row.try_get("schedule_id").map_err(sqlx_err)?,
        expected_at: row.try_get("expected_at").map_err(sqlx_err)?,
        detected_at: row.try_get("detected_at").map_err(sqlx_err)?,
        action_taken: row.try_get("action_taken").map_err(sqlx_err)?,
        created_at: row.try_get("created_at").map_err(sqlx_err)?,
    })
}

fn sqlx_err(e: sqlx::Error) -> AppError {
    AppError::Database(e.to_string())
}

fn validate_schedule_request(req: &CreateScheduleRequest) -> Result<()> {
    if req.tenant_id.trim().is_empty() {
        return Err(AppError::InvalidInput("tenant_id must not be empty".into()));
    }
    if req.agent_name.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "agent_name must not be empty".into(),
        ));
    }
    if req.cron_expression.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "cron_expression must not be empty".into(),
        ));
    }
    parse_timezone(&req.timezone).map_err(AppError::InvalidInput)?;
    validate_grace_period(req.grace_period_seconds)
}

fn validate_grace_period(grace_period_seconds: i32) -> Result<()> {
    if grace_period_seconds < 0 {
        return Err(AppError::InvalidInput(
            "grace_period_seconds must be non-negative".into(),
        ));
    }
    Ok(())
}

fn validate_event_type(t: &str) -> Result<()> {
    const VALID: &[&str] = &[
        "webhook",
        "document_upload",
        "field_change",
        "agent_complete",
    ];
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

    #[test]
    fn compute_next_run_after_honors_non_utc_timezone() {
        let after = DateTime::from_timestamp(1_720_008_000, 0).expect("valid timestamp");
        let utc_next = compute_next_run_after("0 0 9 * * * *", "UTC", after).unwrap();
        let pacific_next =
            compute_next_run_after("0 0 9 * * * *", "America/Los_Angeles", after).unwrap();

        assert_ne!(utc_next, pacific_next);
        assert_eq!(utc_next, 1_720_083_600);
        assert_eq!(pacific_next, 1_720_022_400);
    }

    #[test]
    fn compute_next_run_after_accepts_posix_five_field_daily_cron() {
        let after = DateTime::from_timestamp(1_720_008_000, 0).expect("valid timestamp");
        let posix = compute_next_run_after("0 9 * * *", "UTC", after).unwrap();
        let cron_native = compute_next_run_after("0 0 9 * * * *", "UTC", after).unwrap();
        assert_eq!(posix, cron_native);
        assert_eq!(posix, 1_720_083_600);
    }

    #[test]
    fn normalize_cron_expression_only_allocates_for_five_fields() {
        assert!(matches!(
            normalize_cron_expression("0 0 9 * * * *"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert!(matches!(
            normalize_cron_expression("@daily"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(normalize_cron_expression("0 9 * * *"), "0 0 9 * * *");
    }

    #[test]
    fn compute_next_run_after_rejects_invalid_timezone() {
        let after = DateTime::from_timestamp(1_720_008_000, 0).expect("valid timestamp");
        let err = compute_next_run_after("0 0 9 * * * *", "Not/AZone", after).unwrap_err();
        assert!(err.contains("Invalid timezone"));
    }

    #[test]
    fn validate_grace_period_rejects_negative_values() {
        assert!(validate_grace_period(0).is_ok());
        assert!(validate_grace_period(120).is_ok());
        assert!(validate_grace_period(-1).is_err());
    }

    #[test]
    fn update_pipeline_sql_is_tenant_scoped() {
        let sql = update_pipeline_sql();
        assert!(sql.contains("tenant_id = $1"));
        assert!(sql.contains("id = $2"));
    }

    #[test]
    fn update_schedule_sql_is_tenant_scoped() {
        let sql = update_schedule_where_clause_sql();
        assert!(sql.contains("id = $1"));
        assert!(sql.contains("tenant_id = $2"));
    }

    #[test]
    fn delete_schedule_for_tenant_sql_is_tenant_scoped() {
        let sql = delete_schedule_for_tenant_sql();
        assert!(sql.contains("id = $1"));
        assert!(sql.contains("tenant_id = $2"));
    }

    #[test]
    fn list_missed_runs_for_tenant_sql_is_tenant_scoped() {
        let sql = list_missed_runs_for_tenant_sql();
        assert!(sql.contains("JOIN agent_schedules"));
        assert!(sql.contains("agent_schedules.tenant_id = $1"));
        assert!(sql.contains("missed_runs.schedule_id = $2"));
    }

    #[test]
    fn delete_pipeline_for_tenant_sql_is_tenant_scoped() {
        let sql = delete_pipeline_for_tenant_sql();
        assert!(sql.contains("tenant_id = $1"));
        assert!(sql.contains("id = $2"));
    }

    #[test]
    fn update_missed_run_action_sql_targets_schedule_slot() {
        let sql = update_missed_run_action_sql();
        assert!(sql.contains("schedule_id = $1"));
        assert!(sql.contains("expected_at = $2"));
        assert!(sql.contains("action_taken = $3"));
    }

    #[test]
    fn insert_missed_run_audit_sql_is_atomic_per_schedule_slot() {
        let sql = insert_missed_run_audit_sql();
        assert!(sql.contains("ON CONFLICT (schedule_id, expected_at) DO NOTHING"));
        assert!(!sql.contains("WHERE NOT EXISTS"));
    }

    #[test]
    fn validate_schedule_request_requires_tenant_agent_and_cron() {
        let mut req = CreateScheduleRequest {
            tenant_id: "tenant-1".to_string(),
            agent_name: "agent-a".to_string(),
            cron_expression: "0 0/5 * * * * *".to_string(),
            timezone: "UTC".to_string(),
            enabled: true,
            grace_period_seconds: 120,
        };
        assert!(validate_schedule_request(&req).is_ok());
        req.tenant_id.clear();
        assert!(validate_schedule_request(&req).is_err());
        req.tenant_id = "tenant-1".to_string();
        req.agent_name.clear();
        assert!(validate_schedule_request(&req).is_err());
        req.agent_name = "agent-a".to_string();
        req.cron_expression.clear();
        assert!(validate_schedule_request(&req).is_err());

        req.cron_expression = "0 0/5 * * * * *".to_string();
        req.timezone = "Not/AZone".to_string();
        assert!(validate_schedule_request(&req).is_err());
    }
}
