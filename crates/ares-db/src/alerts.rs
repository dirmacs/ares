use crate::query_builders::{
    build_list_alerts_sql, list_alerts_select_sql, ACTIVE_ALERT_COUNT_SQL,
    CREATE_ALERT_SQL, RESOLVE_ALERT_SQL,
};
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

    sqlx::query(CREATE_ALERT_SQL)
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
    let _dynamic = build_list_alerts_sql(severity_filter, resolved_filter, limit);

    let rows = match (severity_filter, resolved_filter) {
        (Some(sev), Some(res)) => {
            sqlx::query(list_alerts_select_sql(Some(sev), Some(res)))
            .bind(sev)
            .bind(res)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        (Some(sev), None) => {
            sqlx::query(list_alerts_select_sql(Some(sev), None))
            .bind(sev)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        (None, Some(res)) => {
            sqlx::query(list_alerts_select_sql(None, Some(res)))
            .bind(res)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        (None, None) => {
            sqlx::query(list_alerts_select_sql(None, None))
            .bind(limit)
            .fetch_all(pool)
            .await
        }
    }
    .map_err(|e| AppError::Database(e.to_string()))?;

    rows.iter()
        .map(|row| {
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
        })
        .collect()
}

pub async fn resolve_alert(pool: &PgPool, alert_id: &str, resolved_by: Option<&str>) -> Result<()> {
    let now = now_ts();

    let result = sqlx::query(RESOLVE_ALERT_SQL)
    .bind(now)
    .bind(resolved_by)
    .bind(alert_id)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "Alert not found or already resolved".to_string(),
        ));
    }

    Ok(())
}

pub async fn get_active_alert_count(pool: &PgPool) -> Result<i64> {
    let row = sqlx::query(ACTIVE_ALERT_COUNT_SQL)
        .fetch_one(pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(row.get("cnt"))
}


#[cfg(test)]
mod alert_tests {
    use super::*;
    use crate::query_builders::{build_list_alerts_sql, list_alerts_select_sql};


    use sqlx::postgres::PgPoolOptions;
    use sqlx::PgPool;

    fn unreachable_postgres_pool() -> PgPool {
        PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/nope")
            .expect("connect_lazy should not fail for malformed URLs")
    }

    fn assert_database_error<T: std::fmt::Debug>(result: Result<T>) {
        matches::assert_matches!(
            result.unwrap_err(),
            AppError::Database(msg) if !msg.is_empty()
        );
    }

    // ---- SQL constants (no live Postgres) --------------------------------

    #[test]
    fn create_alert_sql_inserts_unresolved_row() {
        assert!(CREATE_ALERT_SQL.contains("INSERT INTO alerts"));
        assert!(CREATE_ALERT_SQL.contains("FALSE"));
        assert!(CREATE_ALERT_SQL.contains("$6"));
    }

    #[test]
    fn resolve_alert_sql_requires_unresolved_target() {
        assert!(RESOLVE_ALERT_SQL.contains("resolved = TRUE"));
        assert!(RESOLVE_ALERT_SQL.contains("WHERE id = $3 AND resolved = FALSE"));
    }

    #[test]
    fn active_alert_count_sql_counts_unresolved_only() {
        assert!(ACTIVE_ALERT_COUNT_SQL.contains("resolved = FALSE"));
        assert!(ACTIVE_ALERT_COUNT_SQL.contains("COUNT(*)"));
    }

    // ---- Async DB error mapping (no live Postgres) -----------------------

    #[tokio::test]
    async fn create_alert_maps_execute_error_to_database() {
        let pool = unreachable_postgres_pool();
        assert_database_error(
            create_alert(&pool, "critical", "health", "disk", "full").await,
        );
    }

    #[tokio::test]
    async fn list_alerts_no_filters_maps_fetch_error() {
        let pool = unreachable_postgres_pool();
        assert_database_error(list_alerts(&pool, None, None, 10).await);
    }

    #[tokio::test]
    async fn list_alerts_severity_only_maps_fetch_error() {
        let pool = unreachable_postgres_pool();
        assert_database_error(list_alerts(&pool, Some("warn"), None, 10).await);
    }

    #[tokio::test]
    async fn list_alerts_resolved_only_maps_fetch_error() {
        let pool = unreachable_postgres_pool();
        assert_database_error(list_alerts(&pool, None, Some(false), 10).await);
    }

    #[tokio::test]
    async fn list_alerts_both_filters_maps_fetch_error() {
        let pool = unreachable_postgres_pool();
        assert_database_error(list_alerts(&pool, Some("critical"), Some(true), 5).await);
    }

    #[tokio::test]
    async fn resolve_alert_maps_execute_error_to_database() {
        let pool = unreachable_postgres_pool();
        assert_database_error(resolve_alert(&pool, "missing-id", Some("ops")).await);
    }

    #[tokio::test]
    async fn get_active_alert_count_maps_fetch_error_to_database() {
        let pool = unreachable_postgres_pool();
        assert_database_error(get_active_alert_count(&pool).await);
    }

    // ---- helpers --------------------------------------------------------

    fn sample_alert() -> Alert {
        Alert {
            id: "a1".into(),
            severity: "critical".into(),
            source: "health".into(),
            title: "disk full".into(),
            message: "disk usage at 98%".into(),
            resolved: false,
            created_at: 1_700_000_000,
            resolved_at: None,
            resolved_by: None,
        }
    }

    fn resolved_alert() -> Alert {
        Alert {
            id: "a2".into(),
            severity: "warn".into(),
            source: "quota".into(),
            title: "quota exceeded".into(),
            message: "over 90%".into(),
            resolved: true,
            created_at: 1_700_000_100,
            resolved_at: Some(1_700_000_200),
            resolved_by: Some("admin".into()),
        }
    }

    // ---- now_ts ---------------------------------------------------------

    #[test]
    fn now_ts_returns_reasonable_epoch() {
        let ts = now_ts();
        // Must be after 2020-01-01 (1_577_836_800) and not wildly in the future.
        assert!(ts > 1_577_836_800, "timestamp {ts} predates 2020");
        assert!(ts < 4_000_000_000, "timestamp {ts} is unreasonably far in future");
    }

    // ---- Alert serde: roundtrip -----------------------------------------

    #[test]
    fn alert_serde_roundtrip_none_optionals() {
        let alert = sample_alert();
        let json = serde_json::to_value(&alert).unwrap();
        let restored: Alert = serde_json::from_value(json).unwrap();
        assert_eq!(restored.id, alert.id);
        assert_eq!(restored.severity, alert.severity);
        assert_eq!(restored.source, alert.source);
        assert_eq!(restored.title, alert.title);
        assert_eq!(restored.message, alert.message);
        assert_eq!(restored.resolved, alert.resolved);
        assert_eq!(restored.created_at, alert.created_at);
        assert_eq!(restored.resolved_at, None);
        assert_eq!(restored.resolved_by, None);
    }

    #[test]
    fn alert_serde_roundtrip_some_optionals() {
        let alert = resolved_alert();
        let json = serde_json::to_value(&alert).unwrap();
        let restored: Alert = serde_json::from_value(json).unwrap();
        assert_eq!(restored.resolved, true);
        assert_eq!(restored.resolved_at, Some(1_700_000_200));
        assert_eq!(restored.resolved_by.as_deref(), Some("admin"));
    }

    #[test]
    fn alert_json_keys_match_field_names() {
        let alert = resolved_alert();
        let json = serde_json::to_value(&alert).unwrap();
        // Every struct field must appear as a JSON key.
        for key in &["id", "severity", "source", "title", "message",
                      "resolved", "created_at", "resolved_at", "resolved_by"] {
            assert!(json.get(key).is_some(), "missing key: {key}");
        }
    }

    #[test]
    fn alert_deserialize_ignores_extra_fields() {
        let mut json = serde_json::to_value(sample_alert()).unwrap();
        json["extra_field"] = serde_json::json!("noise");
        let restored: Alert = serde_json::from_value(json).unwrap();
        assert_eq!(restored.id, "a1");
    }

    // ---- Alert field access ---------------------------------------------

    #[test]
    fn alert_fields_are_public_and_writable() {
        let mut alert = sample_alert();
        alert.resolved = true;
        alert.resolved_at = Some(999);
        alert.resolved_by = Some("ops".into());
        assert!(alert.resolved);
        assert_eq!(alert.resolved_at, Some(999));
        assert_eq!(alert.resolved_by.as_deref(), Some("ops"));
    }

    // ---- Alert Clone / Debug --------------------------------------------

    #[test]
    fn alert_clone_produces_independent_copy() {
        let a = sample_alert();
        let mut b = a.clone();
        b.id = "cloned".into();
        assert_eq!(a.id, "a1");
        assert_eq!(b.id, "cloned");
    }

    #[test]
    fn alert_debug_format_contains_id() {
        let alert = sample_alert();
        let dbg = format!("{alert:?}");
        assert!(dbg.contains("a1"));
    }

    // ---- build_list_alerts_sql ------------------------------------------

    #[test]
    fn build_sql_no_filters() {
        let sql = build_list_alerts_sql(None, None, 10);
        assert!(sql.contains("FROM alerts"));
        assert!(!sql.contains("severity ="));
        assert!(!sql.contains("resolved ="));
        assert!(sql.contains("LIMIT 10"));
    }

    #[test]
    fn build_sql_severity_only() {
        let sql = build_list_alerts_sql(Some("warn"), None, 25);
        assert!(sql.contains("severity = $1"));
        assert!(!sql.contains("resolved ="));
        assert!(sql.contains("LIMIT 25"));
    }

    #[test]
    fn build_sql_resolved_only() {
        let sql = build_list_alerts_sql(None, Some(false), 50);
        assert!(!sql.contains("severity ="));
        assert!(sql.contains("resolved = $1"));
        assert!(sql.contains("LIMIT 50"));
    }

    #[test]
    fn build_sql_both_filters() {
        let sql = build_list_alerts_sql(Some("critical"), Some(true), 5);
        assert!(sql.contains("severity = $1"));
        assert!(sql.contains("resolved = $2"));
        assert!(sql.contains("LIMIT 5"));
    }

    #[test]
    fn build_sql_orders_by_created_at_desc() {
        let sql = build_list_alerts_sql(None, None, 1);
        assert!(sql.contains("ORDER BY created_at DESC"));
    }

    #[test]
    fn build_sql_selects_all_alert_columns() {
        let sql = build_list_alerts_sql(None, None, 1);
        for col in &["id", "severity", "source", "title", "message",
                      "resolved", "created_at", "resolved_at", "resolved_by"] {
            assert!(sql.contains(col), "missing column: {col}");
        }
    }

    // ---- list_alerts_select_sql -----------------------------------------

    #[test]
    fn select_sql_both_filters_has_severity_and_resolved() {
        let sql = list_alerts_select_sql(Some("info"), Some(false));
        assert!(sql.contains("severity = $1"));
        assert!(sql.contains("resolved = $2"));
        assert!(sql.contains("LIMIT $3"));
    }

    #[test]
    fn select_sql_severity_only() {
        let sql = list_alerts_select_sql(Some("info"), None);
        assert!(sql.contains("severity = $1"));
        assert!(!sql.contains("resolved ="));
        assert!(sql.contains("LIMIT $2"));
    }

    #[test]
    fn select_sql_resolved_only() {
        let sql = list_alerts_select_sql(None, Some(true));
        assert!(!sql.contains("severity ="));
        assert!(sql.contains("resolved = $1"));
        assert!(sql.contains("LIMIT $2"));
    }

    #[test]
    fn select_sql_no_filters() {
        let sql = list_alerts_select_sql(None, None);
        assert!(!sql.contains("severity ="));
        assert!(!sql.contains("resolved ="));
        assert!(sql.contains("LIMIT $1"));
    }

    #[test]
    fn select_sql_always_starts_with_select() {
        for (sev, res) in [(Some("x"), Some(true)), (Some("x"), None),
                           (None, Some(true)), (None, None)] {
            let sql = list_alerts_select_sql(sev, res);
            assert!(sql.starts_with("SELECT"), "should start with SELECT: {sql}");
        }
    }

    #[test]
    fn select_sql_always_orders_by_created_at_desc() {
        for (sev, res) in [(Some("x"), Some(true)), (Some("x"), None),
                           (None, Some(true)), (None, None)] {
            let sql = list_alerts_select_sql(sev, res);
            assert!(sql.contains("ORDER BY created_at DESC"), "missing ORDER BY: {sql}");
        }
    }
}
