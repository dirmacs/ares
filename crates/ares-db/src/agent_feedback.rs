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

    // ── validate_feedback_type ──────────────────────────────────────────

    #[test]
    fn validates_all_seven_feedback_types() {
        for valid in &[
            "positive",
            "negative",
            "neutral",
            "reviewer_score",
            "safety_flag",
            "unsupported_claim",
            "fallback_quality",
        ] {
            assert!(
                validate_feedback_type(valid).is_ok(),
                "expected '{valid}' to be accepted"
            );
        }
    }

    #[test]
    fn rejects_invalid_feedback_types() {
        for invalid in &["made_up", "", "POSITIVE", "Positive", "positivee"] {
            let err = validate_feedback_type(invalid).unwrap_err();
            match err {
                AppError::InvalidInput(msg) => {
                    assert!(
                        msg.contains(invalid),
                        "error message should quote the bad type: {msg}"
                    );
                }
                other => panic!("expected InvalidInput, got {other:?}"),
            }
        }
    }

    // ── validate_score ──────────────────────────────────────────────────

    #[test]
    fn score_none_is_always_valid() {
        assert!(validate_score(None).is_ok());
    }

    #[test]
    fn score_boundary_values_accepted() {
        assert!(validate_score(Some(0.0)).is_ok());
        assert!(validate_score(Some(1.0)).is_ok());
        assert!(validate_score(Some(0.5)).is_ok());
    }

    #[test]
    fn score_out_of_range_rejected() {
        for bad in &[-0.001, -0.1, -1.0, 1.001, 1.1, 2.0, 100.0] {
            assert!(
                validate_score(Some(*bad)).is_err(),
                "expected {bad} to be rejected"
            );
        }
    }

    #[test]
    fn score_non_finite_rejected() {
        assert!(validate_score(Some(f64::NAN)).is_err());
        assert!(validate_score(Some(f64::INFINITY)).is_err());
        assert!(validate_score(Some(f64::NEG_INFINITY)).is_err());
    }

    #[test]
    fn score_validation_error_message_contains_range() {
        let err = validate_score(Some(1.5)).unwrap_err();
        match err {
            AppError::InvalidInput(msg) => {
                assert!(msg.contains("0.0"), "should mention range: {msg}");
                assert!(msg.contains("1.0"), "should mention range: {msg}");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    // ── AgentRunFeedback serde ──────────────────────────────────────────

    #[test]
    fn agent_run_feedback_roundtrip() {
        let feedback = AgentRunFeedback {
            id: "fb-1".into(),
            tenant_id: "t1".into(),
            agent_name: "agent-1".into(),
            run_id: Some("run-42".into()),
            feedback_type: "positive".into(),
            score: Some(0.85),
            flags: vec!["helpful".into(), "accurate".into()],
            notes: Some("good output".into()),
            reviewer: Some("ops".into()),
            created_at: 1_700_000_000,
        };
        let json = serde_json::to_value(&feedback).unwrap();
        let back: AgentRunFeedback = serde_json::from_value(json).unwrap();
        assert_eq!(back.id, "fb-1");
        assert_eq!(back.tenant_id, "t1");
        assert_eq!(back.agent_name, "agent-1");
        assert_eq!(back.run_id.as_deref(), Some("run-42"));
        assert_eq!(back.feedback_type, "positive");
        assert_eq!(back.score, Some(0.85));
        assert_eq!(back.flags, vec!["helpful", "accurate"]);
        assert_eq!(back.notes.as_deref(), Some("good output"));
        assert_eq!(back.reviewer.as_deref(), Some("ops"));
        assert_eq!(back.created_at, 1_700_000_000);
    }

    #[test]
    fn agent_run_feedback_none_fields_roundtrip() {
        let feedback = AgentRunFeedback {
            id: "fb-2".into(),
            tenant_id: "t2".into(),
            agent_name: "agent-2".into(),
            run_id: None,
            feedback_type: "neutral".into(),
            score: None,
            flags: vec![],
            notes: None,
            reviewer: None,
            created_at: 1_700_000_100,
        };
        let json = serde_json::to_value(&feedback).unwrap();
        assert!(json["run_id"].is_null());
        assert!(json["score"].is_null());
        assert!(json["notes"].is_null());
        assert!(json["reviewer"].is_null());
        assert_eq!(json["flags"], serde_json::json!([]));
        let back: AgentRunFeedback = serde_json::from_value(json).unwrap();
        assert!(back.run_id.is_none());
        assert!(back.score.is_none());
        assert!(back.flags.is_empty());
    }

    #[test]
    fn agent_run_feedback_deserializes_without_flags_key() {
        let json = serde_json::json!({
            "id": "fb-3",
            "tenant_id": "t3",
            "agent_name": "agent-3",
            "feedback_type": "negative",
            "score": 0.2,
            "notes": null,
            "created_at": 1_700_000_200
        });
        let feedback: AgentRunFeedback = serde_json::from_value(json).unwrap();
        assert!(feedback.flags.is_empty(), "missing flags should default to empty vec");
    }

    #[test]
    fn agent_run_feedback_json_keys() {
        let feedback = AgentRunFeedback {
            id: "fb-s".into(),
            tenant_id: "t".into(),
            agent_name: "a".into(),
            run_id: Some("r".into()),
            feedback_type: "reviewer_score".into(),
            score: Some(0.0),
            flags: vec![],
            notes: Some("n".into()),
            reviewer: Some("rev".into()),
            created_at: 42,
        };
        let json = serde_json::to_value(&feedback).unwrap();
        assert_eq!(json["id"], "fb-s");
        assert_eq!(json["tenant_id"], "t");
        assert_eq!(json["agent_name"], "a");
        assert_eq!(json["run_id"], "r");
        assert_eq!(json["feedback_type"], "reviewer_score");
        assert_eq!(json["score"], 0.0);
        assert_eq!(json["notes"], "n");
        assert_eq!(json["reviewer"], "rev");
        assert_eq!(json["created_at"], 42);
    }

    // ── NewAgentRunFeedback serde ───────────────────────────────────────

    #[test]
    fn new_agent_run_feedback_roundtrip() {
        let new = NewAgentRunFeedback {
            tenant_id: "t10".into(),
            agent_name: "agent-10".into(),
            run_id: Some("r-10".into()),
            feedback_type: "safety_flag".into(),
            score: Some(0.3),
            flags: vec!["flagged".into()],
            notes: Some("unsafe claim".into()),
            reviewer: None,
        };
        let json = serde_json::to_value(&new).unwrap();
        let back: NewAgentRunFeedback = serde_json::from_value(json).unwrap();
        assert_eq!(back.tenant_id, "t10");
        assert_eq!(back.feedback_type, "safety_flag");
        assert_eq!(back.flags, vec!["flagged"]);
    }

    #[test]
    fn new_agent_run_feedback_deserializes_without_flags_key() {
        let json = serde_json::json!({
            "tenant_id": "t11",
            "agent_name": "agent-11",
            "feedback_type": "neutral"
        });
        let new: NewAgentRunFeedback = serde_json::from_value(json).unwrap();
        assert!(new.flags.is_empty());
        assert!(new.run_id.is_none());
        assert!(new.score.is_none());
    }

    #[test]
    fn new_agent_run_feedback_json_structure() {
        let new = NewAgentRunFeedback {
            tenant_id: "t".into(),
            agent_name: "a".into(),
            run_id: Some("r".into()),
            feedback_type: "fallback_quality".into(),
            score: Some(1.0),
            flags: vec!["f1".into()],
            notes: Some("n".into()),
            reviewer: Some("rev".into()),
        };
        let json = serde_json::to_value(&new).unwrap();
        assert_eq!(json["tenant_id"], "t");
        assert_eq!(json["agent_name"], "a");
        assert_eq!(json["run_id"], "r");
        assert_eq!(json["feedback_type"], "fallback_quality");
        assert_eq!(json["score"], 1.0);
        assert_eq!(json["flags"][0], "f1");
        assert_eq!(json["notes"], "n");
        assert_eq!(json["reviewer"], "rev");
        // NewAgentRunFeedback has no id or created_at
        assert!(json.get("id").is_none());
        assert!(json.get("created_at").is_none());
    }

    // ── AgentFeedbackSummary serde + PartialEq ──────────────────────────

    #[test]
    fn agent_feedback_summary_roundtrip() {
        let summary = AgentFeedbackSummary {
            tenant_id: "t20".into(),
            agent_name: "agent-20".into(),
            days: 30,
            total_feedback: 150,
            positive_count: 120,
            negative_count: 15,
            score_count: 100,
            average_score: Some(0.82),
            flags: vec!["fallback_quality".into()],
        };
        let json = serde_json::to_value(&summary).unwrap();
        let back: AgentFeedbackSummary = serde_json::from_value(json).unwrap();
        assert_eq!(summary, back, "roundtrip should produce equal struct");
    }

    #[test]
    fn agent_feedback_summary_no_scores_roundtrip() {
        let summary = AgentFeedbackSummary {
            tenant_id: "t21".into(),
            agent_name: "agent-21".into(),
            days: 7,
            total_feedback: 0,
            positive_count: 0,
            negative_count: 0,
            score_count: 0,
            average_score: None,
            flags: vec![],
        };
        let json = serde_json::to_value(&summary).unwrap();
        assert!(json["average_score"].is_null());
        assert_eq!(json["total_feedback"], 0);
        let back: AgentFeedbackSummary = serde_json::from_value(json).unwrap();
        assert_eq!(summary, back);
    }

    #[test]
    fn agent_feedback_summary_deserializes_without_flags_key() {
        let json = serde_json::json!({
            "tenant_id": "t22",
            "agent_name": "agent-22",
            "days": 1,
            "total_feedback": 5,
            "positive_count": 3,
            "negative_count": 2,
            "score_count": 5,
            "average_score": 0.6
        });
        let summary: AgentFeedbackSummary = serde_json::from_value(json).unwrap();
        assert!(summary.flags.is_empty());
    }

    #[test]
    fn agent_feedback_summary_json_keys() {
        let summary = AgentFeedbackSummary {
            tenant_id: "t".into(),
            agent_name: "a".into(),
            days: 30,
            total_feedback: 100,
            positive_count: 80,
            negative_count: 10,
            score_count: 90,
            average_score: Some(0.75),
            flags: vec!["safety_flag".into()],
        };
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["tenant_id"], "t");
        assert_eq!(json["agent_name"], "a");
        assert_eq!(json["days"], 30);
        assert_eq!(json["total_feedback"], 100);
        assert_eq!(json["positive_count"], 80);
        assert_eq!(json["negative_count"], 10);
        assert_eq!(json["score_count"], 90);
        assert_eq!(json["average_score"], 0.75);
        assert_eq!(json["flags"][0], "safety_flag");
    }

    // ── Debug and Clone traits ──────────────────────────────────────────

    #[test]
    fn agent_run_feedback_debug_and_clone() {
        let feedback = AgentRunFeedback {
            id: "fb-d".into(),
            tenant_id: "t".into(),
            agent_name: "a".into(),
            run_id: None,
            feedback_type: "positive".into(),
            score: Some(0.5),
            flags: vec![],
            notes: None,
            reviewer: None,
            created_at: 0,
        };
        let _dbg = format!("{:?}", feedback);
        let cloned = feedback.clone();
        assert_eq!(feedback.id, cloned.id);
        assert_eq!(feedback.feedback_type, cloned.feedback_type);
    }

    #[test]
    fn new_agent_run_feedback_debug_and_clone() {
        let new = NewAgentRunFeedback {
            tenant_id: "t".into(),
            agent_name: "a".into(),
            run_id: None,
            feedback_type: "negative".into(),
            score: None,
            flags: vec![],
            notes: None,
            reviewer: None,
        };
        let _dbg = format!("{:?}", new);
        let cloned = new.clone();
        assert_eq!(new.tenant_id, cloned.tenant_id);
    }

    #[test]
    fn agent_feedback_summary_debug_and_clone() {
        let summary = AgentFeedbackSummary {
            tenant_id: "t".into(),
            agent_name: "a".into(),
            days: 30,
            total_feedback: 10,
            positive_count: 8,
            negative_count: 2,
            score_count: 10,
            average_score: Some(0.8),
            flags: vec![],
        };
        let _dbg = format!("{:?}", summary);
        let cloned = summary.clone();
        assert_eq!(summary, cloned);
    }

    // ── now_ts ──────────────────────────────────────────────────────────

    #[test]
    fn now_ts_returns_reasonable_timestamp() {
        let ts = now_ts();
        // After 2020-01-01 (1_577_836_800), before year 2100
        assert!(ts > 1_577_836_800, "timestamp too old: {ts}");
        assert!(ts < 4_102_444_800, "timestamp too far in future: {ts}");
    }
}
