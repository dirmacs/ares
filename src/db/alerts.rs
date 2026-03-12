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
pub struct Alert {
    pub id: String,
    pub severity: String,
    pub source: String,
    pub title: String,
    pub message: String,
    pub resolved: bool,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
    pub resolved_by: Option<String>,
}

pub async fn create_alert(
    pool: &PgPool,
    severity: &str,
    source: &str,
    title: &str,
    message: &str,
) -> Result<Alert> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ts();

    sqlx::query(
        "INSERT INTO alerts (id, severity, source, title, message, resolved, created_at)
         VALUES ($1, $2, $3, $4, $5, FALSE, $6)"
    )
    .bind(&id)
    .bind(severity)
    .bind(source)
    .bind(title)
    .bind(message)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(Alert {
        id,
        severity: severity.to_string(),
        source: source.to_string(),
        title: title.to_string(),
        message: message.to_string(),
        resolved: false,
        created_at: now,
        resolved_at: None,
        resolved_by: None,
    })
}

pub async fn list_alerts(
    pool: &PgPool,
    severity_filter: Option<&str>,
    resolved_filter: Option<bool>,
    limit: i64,
) -> Result<Vec<Alert>> {
    // Build query dynamically based on filters
    let mut query = String::from(
        "SELECT id, severity, source, title, message, resolved, created_at, resolved_at, resolved_by
         FROM alerts WHERE 1=1"
    );
    let mut bind_idx = 1;

    if severity_filter.is_some() {
        query.push_str(&format!(" AND severity = ${}", bind_idx));
        bind_idx += 1;
    }
    if resolved_filter.is_some() {
        query.push_str(&format!(" AND resolved = ${}", bind_idx));
        bind_idx += 1;
    }
    let _ = bind_idx;

    query.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

    // Since sqlx doesn't support truly dynamic queries easily, use separate branches
    let rows = match (severity_filter, resolved_filter) {
        (Some(sev), Some(res)) => {
            sqlx::query(
                "SELECT id, severity, source, title, message, resolved, created_at, resolved_at, resolved_by
                 FROM alerts WHERE severity = $1 AND resolved = $2
                 ORDER BY created_at DESC LIMIT $3"
            )
            .bind(sev)
            .bind(res)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        (Some(sev), None) => {
            sqlx::query(
                "SELECT id, severity, source, title, message, resolved, created_at, resolved_at, resolved_by
                 FROM alerts WHERE severity = $1
                 ORDER BY created_at DESC LIMIT $2"
            )
            .bind(sev)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        (None, Some(res)) => {
            sqlx::query(
                "SELECT id, severity, source, title, message, resolved, created_at, resolved_at, resolved_by
                 FROM alerts WHERE resolved = $1
                 ORDER BY created_at DESC LIMIT $2"
            )
            .bind(res)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        (None, None) => {
            sqlx::query(
                "SELECT id, severity, source, title, message, resolved, created_at, resolved_at, resolved_by
                 FROM alerts ORDER BY created_at DESC LIMIT $1"
            )
            .bind(limit)
            .fetch_all(pool)
            .await
        }
    }
    .map_err(|e| AppError::Database(e.to_string()))?;

    rows.iter().map(|row| {
        Ok(Alert {
            id: row.get("id"),
            severity: row.get("severity"),
            source: row.get("source"),
            title: row.get("title"),
            message: row.get("message"),
            resolved: row.get("resolved"),
            created_at: row.get("created_at"),
            resolved_at: row.get("resolved_at"),
            resolved_by: row.get("resolved_by"),
        })
    }).collect()
}

pub async fn resolve_alert(pool: &PgPool, alert_id: &str, resolved_by: Option<&str>) -> Result<()> {
    let now = now_ts();

    let result = sqlx::query(
        "UPDATE alerts SET resolved = TRUE, resolved_at = $1, resolved_by = $2 WHERE id = $3 AND resolved = FALSE"
    )
    .bind(now)
    .bind(resolved_by)
    .bind(alert_id)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Alert not found or already resolved".to_string()));
    }

    Ok(())
}

pub async fn get_active_alert_count(pool: &PgPool) -> Result<i64> {
    let row = sqlx::query("SELECT COUNT(*) as cnt FROM alerts WHERE resolved = FALSE")
        .fetch_one(pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(row.get("cnt"))
}
