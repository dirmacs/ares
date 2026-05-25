use crate::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Admin-supplied quality feedback for a managed agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunFeedback {
    pub id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub run_id: Option<String>,
    pub feedback_type: String,
    pub score: Option<f64>,
    #[serde(default)]
    pub flags: Vec<String>,
    pub notes: Option<String>,
    pub reviewer: Option<String>,
    pub created_at: i64,
}

/// New run feedback payload after tenant/agent/run scoping has been checked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewAgentRunFeedback {
    pub tenant_id: String,
    pub agent_name: String,
    pub run_id: Option<String>,
    pub feedback_type: String,
    pub score: Option<f64>,
    #[serde(default)]
    pub flags: Vec<String>,
    pub notes: Option<String>,
    pub reviewer: Option<String>,
}

/// Aggregate quality feedback for a tenant agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentFeedbackSummary {
    pub tenant_id: String,
    pub agent_name: String,
    pub days: i64,
    pub total_feedback: i64,
    pub positive_count: i64,
    pub negative_count: i64,
    pub score_count: i64,
    pub average_score: Option<f64>,
    #[serde(default)]
    pub flags: Vec<String>,
}

pub async fn insert_agent_run_feedback(
    pool: &PgPool,
    feedback: NewAgentRunFeedback,
) -> Result<AgentRunFeedback> {
    validate_feedback_type(&feedback.feedback_type)?;
    validate_score(feedback.score)?;

    if let Some(run_id) = feedback.run_id.as_deref() {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(
                SELECT 1 FROM agent_runs
                WHERE id = $1 AND tenant_id = $2 AND agent_name = $3
             )",
        )
        .bind(run_id)
        .bind(&feedback.tenant_id)
        .bind(&feedback.agent_name)
        .fetch_one(pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

        if !exists {
            return Err(AppError::NotFound(format!(
                "Run '{}' not found for tenant '{}' agent '{}'",
                run_id, feedback.tenant_id, feedback.agent_name
            )));
        }
    }

    let record = AgentRunFeedback {
        id: uuid::Uuid::new_v4().to_string(),
        tenant_id: feedback.tenant_id,
        agent_name: feedback.agent_name,
        run_id: feedback.run_id,
        feedback_type: feedback.feedback_type,
        score: feedback.score,
        flags: feedback.flags,
        notes: feedback.notes,
        reviewer: feedback.reviewer,
        created_at: now_ts(),
    };

    sqlx::query(
        "INSERT INTO agent_run_feedback (
            id, tenant_id, agent_name, run_id, feedback_type, score, flags,
            notes, reviewer, created_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&record.id)
    .bind(&record.tenant_id)
    .bind(&record.agent_name)
    .bind(&record.run_id)
    .bind(&record.feedback_type)
    .bind(record.score)
    .bind(&record.flags)
    .bind(&record.notes)
    .bind(&record.reviewer)
    .bind(record.created_at)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(record)
}

pub async fn get_agent_feedback_summary(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
    days: i64,
) -> Result<AgentFeedbackSummary> {
    let days = days.clamp(1, 366);
    let since = now_ts() - (days * 86_400);
    let row = sqlx::query(
        "SELECT
            COUNT(*)::BIGINT AS total_feedback,
            COUNT(*) FILTER (WHERE feedback_type = 'positive')::BIGINT AS positive_count,
            COUNT(*) FILTER (WHERE feedback_type = 'negative')::BIGINT AS negative_count,
            COUNT(score)::BIGINT AS score_count,
            AVG(score)::DOUBLE PRECISION AS average_score
         FROM agent_run_feedback
         WHERE tenant_id = $1 AND agent_name = $2 AND created_at >= $3",
    )
    .bind(tenant_id)
    .bind(agent_name)
    .bind(since)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    let flag_rows = sqlx::query(
        "SELECT DISTINCT flag
         FROM agent_run_feedback, unnest(flags) AS flag
         WHERE tenant_id = $1 AND agent_name = $2 AND created_at >= $3
         ORDER BY flag ASC
         LIMIT 20",
    )
    .bind(tenant_id)
    .bind(agent_name)
    .bind(since)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(AgentFeedbackSummary {
        tenant_id: tenant_id.to_string(),
        agent_name: agent_name.to_string(),
        days,
        total_feedback: row.try_get("total_feedback").unwrap_or(0),
        positive_count: row.try_get("positive_count").unwrap_or(0),
        negative_count: row.try_get("negative_count").unwrap_or(0),
        score_count: row.try_get("score_count").unwrap_or(0),
        average_score: row.try_get("average_score").ok(),
        flags: flag_rows
            .iter()
            .filter_map(|row| row.try_get::<String, _>("flag").ok())
            .collect(),
    })
}

fn validate_feedback_type(feedback_type: &str) -> Result<()> {
    match feedback_type {
        "positive" | "negative" | "neutral" | "reviewer_score" | "safety_flag"
        | "unsupported_claim" | "fallback_quality" => Ok(()),
        other => Err(AppError::InvalidInput(format!(
            "Invalid feedback_type '{}'",
            other
        ))),
    }
}

fn validate_score(score: Option<f64>) -> Result<()> {
    if let Some(score) = score {
        if !(0.0..=1.0).contains(&score) || !score.is_finite() {
            return Err(AppError::InvalidInput(
                "score must be a finite value between 0.0 and 1.0".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_allowed_feedback_types() {
        assert!(validate_feedback_type("positive").is_ok());
        assert!(validate_feedback_type("unsupported_claim").is_ok());
        assert!(validate_feedback_type("made_up").is_err());
    }

    #[test]
    fn validates_score_range() {
        assert!(validate_score(None).is_ok());
        assert!(validate_score(Some(0.0)).is_ok());
        assert!(validate_score(Some(1.0)).is_ok());
        assert!(validate_score(Some(1.2)).is_err());
        assert!(validate_score(Some(f64::NAN)).is_err());
    }

    #[test]
    fn feedback_record_serializes_flags() {
        let feedback = AgentRunFeedback {
            id: "fb-1".into(),
            tenant_id: "tenant-1".into(),
            agent_name: "agent-1".into(),
            run_id: Some("run-1".into()),
            feedback_type: "positive".into(),
            score: Some(1.0),
            flags: vec!["helpful".into()],
            notes: None,
            reviewer: Some("ops".into()),
            created_at: 1_700_000_000,
        };
        let json = serde_json::to_value(&feedback).unwrap();
        assert_eq!(json["flags"][0], "helpful");
        assert_eq!(json["score"], 1.0);
    }
}
