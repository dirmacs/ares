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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub id: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub details: Option<String>,
    pub admin_ip: Option<String>,
    pub created_at: i64,
}

pub async fn log_admin_action(
    pool: &PgPool,
    action: &str,
    resource_type: &str,
    resource_id: &str,
    details: Option<&str>,
    admin_ip: Option<&str>,
) -> Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ts();

    sqlx::query(
        "INSERT INTO admin_audit_log (id, action, resource_type, resource_id, details, admin_ip, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)"
    )
    .bind(&id)
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(details)
    .bind(admin_ip)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(())
}

pub async fn list_audit_log(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<AuditLogEntry>> {
    let rows = sqlx::query(
        "SELECT id, action, resource_type, resource_id, details, admin_ip, created_at
         FROM admin_audit_log ORDER BY created_at DESC LIMIT $1 OFFSET $2",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    rows.iter()
        .map(|row| {
            Ok(AuditLogEntry {
                id: row.get("id"),
                action: row.get("action"),
                resource_type: row.get("resource_type"),
                resource_id: row.get("resource_id"),
                details: row.get("details"),
                admin_ip: row.get("admin_ip"),
                created_at: row.get("created_at"),
            })
        })
        .collect()
}
