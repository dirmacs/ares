use ares_types::types::{AppError, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use sqlx::postgres::PgPoolOptions;
use std::time::{SystemTime, UNIX_EPOCH};

const INSERT_ADMIN_AUDIT_LOG_SQL: &str = "\
INSERT INTO admin_audit_log (id, action, resource_type, resource_id, details, admin_ip, created_at)
 VALUES ($1, $2, $3, $4, $5, $6, $7)";

const LIST_ADMIN_AUDIT_LOG_SQL: &str = "\
SELECT id, action, resource_type, resource_id, details, admin_ip, created_at
 FROM admin_audit_log ORDER BY created_at DESC LIMIT $1 OFFSET $2";

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

    sqlx::query(INSERT_ADMIN_AUDIT_LOG_SQL)
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
    let rows = sqlx::query(LIST_ADMIN_AUDIT_LOG_SQL)
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- helpers --------------------------------------------------------

    fn sample_entry_none_optionals() -> AuditLogEntry {
        AuditLogEntry {
            id: "log-001".into(),
            action: "create".into(),
            resource_type: "tenant".into(),
            resource_id: "t-abc".into(),
            details: None,
            admin_ip: None,
            created_at: 1_700_000_000,
        }
    }

    fn sample_entry_some_optionals() -> AuditLogEntry {
        AuditLogEntry {
            id: "log-002".into(),
            action: "delete".into(),
            resource_type: "agent".into(),
            resource_id: "agent-9".into(),
            details: Some("removed stale config".into()),
            admin_ip: Some("203.0.113.10".into()),
            created_at: 1_700_000_100,
        }
    }

    fn unreachable_postgres_pool() -> PgPool {
        PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/nope")
            .expect("connect_lazy should not fail for malformed URLs")
    }

    // ---- now_ts ---------------------------------------------------------

    #[test]
    fn now_ts_returns_positive_value() {
        let ts = now_ts();
        assert!(ts > 0, "now_ts should return a positive timestamp, got {ts}");
    }

    #[test]
    fn now_ts_returns_reasonable_epoch() {
        let ts = now_ts();
        assert!(ts > 1_577_836_800, "timestamp {ts} predates 2020");
        assert!(ts < 4_000_000_000, "timestamp {ts} is unreasonably far in future");
    }

    #[test]
    fn now_ts_calls_are_non_decreasing() {
        let a = now_ts();
        let b = now_ts();
        assert!(b >= a, "consecutive calls should be non-decreasing");
    }

    // ---- AuditLogEntry serde: roundtrip ---------------------------------

    #[test]
    fn audit_log_entry_serde_roundtrip_none_optionals() {
        let entry = sample_entry_none_optionals();
        let json = serde_json::to_value(&entry).unwrap();
        let restored: AuditLogEntry = serde_json::from_value(json).unwrap();
        assert_eq!(restored.id, entry.id);
        assert_eq!(restored.action, entry.action);
        assert_eq!(restored.resource_type, entry.resource_type);
        assert_eq!(restored.resource_id, entry.resource_id);
        assert_eq!(restored.details, None);
        assert_eq!(restored.admin_ip, None);
        assert_eq!(restored.created_at, entry.created_at);
    }

    #[test]
    fn audit_log_entry_serde_roundtrip_some_optionals() {
        let entry = sample_entry_some_optionals();
        let json = serde_json::to_string(&entry).unwrap();
        let restored: AuditLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.details.as_deref(), Some("removed stale config"));
        assert_eq!(restored.admin_ip.as_deref(), Some("203.0.113.10"));
        assert_eq!(restored.created_at, 1_700_000_100);
    }

    #[test]
    fn audit_log_entry_json_keys_match_field_names() {
        let entry = sample_entry_some_optionals();
        let json = serde_json::to_value(&entry).unwrap();
        for key in &[
            "id",
            "action",
            "resource_type",
            "resource_id",
            "details",
            "admin_ip",
            "created_at",
        ] {
            assert!(json.get(key).is_some(), "missing key: {key}");
        }
    }

    #[test]
    fn audit_log_entry_none_optionals_serialize_as_null() {
        let entry = sample_entry_none_optionals();
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json["details"].is_null());
        assert!(json["admin_ip"].is_null());
    }

    #[test]
    fn audit_log_entry_deserialize_ignores_extra_fields() {
        let mut json = serde_json::to_value(sample_entry_none_optionals()).unwrap();
        json["extra_field"] = serde_json::json!("noise");
        let restored: AuditLogEntry = serde_json::from_value(json).unwrap();
        assert_eq!(restored.id, "log-001");
    }

    #[test]
    fn audit_log_entry_empty_string_fields_roundtrip() {
        let entry = AuditLogEntry {
            id: String::new(),
            action: String::new(),
            resource_type: String::new(),
            resource_id: String::new(),
            details: Some(String::new()),
            admin_ip: Some(String::new()),
            created_at: 0,
        };
        let restored: AuditLogEntry =
            serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
        assert_eq!(restored.id, "");
        assert_eq!(restored.details.as_deref(), Some(""));
        assert_eq!(restored.created_at, 0);
    }

    #[test]
    fn audit_log_entries_vec_serde_roundtrip_empty() {
        let entries: Vec<AuditLogEntry> = vec![];
        let json = serde_json::to_string(&entries).unwrap();
        let restored: Vec<AuditLogEntry> = serde_json::from_str(&json).unwrap();
        assert!(restored.is_empty());
    }

    #[test]
    fn audit_log_entries_vec_serde_roundtrip_multiple() {
        let entries = vec![
            sample_entry_none_optionals(),
            sample_entry_some_optionals(),
        ];
        let json = serde_json::to_string(&entries).unwrap();
        let restored: Vec<AuditLogEntry> = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].id, "log-001");
        assert_eq!(restored[1].id, "log-002");
    }

    // ---- AuditLogEntry serde: error handling ----------------------------

    #[test]
    fn audit_log_entry_deserialize_rejects_missing_id() {
        let json = r#"{
            "action": "create",
            "resource_type": "tenant",
            "resource_id": "t-1",
            "created_at": 1
        }"#;
        let err = serde_json::from_str::<AuditLogEntry>(json).unwrap_err();
        assert!(
            err.to_string().contains("id"),
            "error should mention missing field: {err}"
        );
    }

    #[test]
    fn audit_log_entry_deserialize_rejects_wrong_created_at_type() {
        let json = r#"{
            "id": "log-1",
            "action": "create",
            "resource_type": "tenant",
            "resource_id": "t-1",
            "created_at": "not-a-number"
        }"#;
        assert!(serde_json::from_str::<AuditLogEntry>(json).is_err());
    }

    #[test]
    fn audit_log_entry_deserialize_rejects_null_action() {
        let json = r#"{
            "id": "log-1",
            "action": null,
            "resource_type": "tenant",
            "resource_id": "t-1",
            "created_at": 1
        }"#;
        assert!(serde_json::from_str::<AuditLogEntry>(json).is_err());
    }

    #[test]
    fn audit_log_entry_deserialize_rejects_malformed_json() {
        let err = serde_json::from_str::<AuditLogEntry>("{not json}").unwrap_err();
        assert!(
            err.is_syntax() || err.is_data(),
            "expected syntax or data error, got {err}"
        );
    }

    #[test]
    fn audit_log_entries_vec_deserialize_rejects_non_array() {
        let err = serde_json::from_str::<Vec<AuditLogEntry>>("{\"id\":\"x\"}").unwrap_err();
        assert!(
            err.is_data() || err.to_string().contains("array"),
            "expected array type mismatch: {err}"
        );
    }

    // ---- AuditLogEntry field access / Clone / Debug ---------------------

    #[test]
    fn audit_log_entry_fields_are_public_and_writable() {
        let mut entry = sample_entry_none_optionals();
        entry.action = "update".into();
        entry.details = Some("patched".into());
        entry.admin_ip = Some("10.0.0.1".into());
        assert_eq!(entry.action, "update");
        assert_eq!(entry.details.as_deref(), Some("patched"));
        assert_eq!(entry.admin_ip.as_deref(), Some("10.0.0.1"));
    }

    #[test]
    fn audit_log_entry_clone_produces_independent_copy() {
        let a = sample_entry_none_optionals();
        let mut b = a.clone();
        b.id = "cloned".into();
        assert_eq!(a.id, "log-001");
        assert_eq!(b.id, "cloned");
    }

    #[test]
    fn audit_log_entry_debug_format_contains_id() {
        let entry = sample_entry_none_optionals();
        let dbg = format!("{entry:?}");
        assert!(dbg.contains("log-001"));
        assert!(dbg.contains("create"));
    }

    // ---- SQL constants / query building ---------------------------------

    #[test]
    fn insert_sql_targets_admin_audit_log_table() {
        assert!(INSERT_ADMIN_AUDIT_LOG_SQL.contains("INSERT INTO admin_audit_log"));
    }

    #[test]
    fn insert_sql_binds_all_seven_columns() {
        for col in &[
            "id",
            "action",
            "resource_type",
            "resource_id",
            "details",
            "admin_ip",
            "created_at",
        ] {
            assert!(
                INSERT_ADMIN_AUDIT_LOG_SQL.contains(col),
                "missing column in INSERT: {col}"
            );
        }
        assert!(INSERT_ADMIN_AUDIT_LOG_SQL.contains("$7"));
    }

    #[test]
    fn list_sql_orders_by_created_at_desc() {
        assert!(LIST_ADMIN_AUDIT_LOG_SQL.contains("ORDER BY created_at DESC"));
    }

    #[test]
    fn list_sql_uses_limit_and_offset_placeholders() {
        assert!(LIST_ADMIN_AUDIT_LOG_SQL.contains("LIMIT $1"));
        assert!(LIST_ADMIN_AUDIT_LOG_SQL.contains("OFFSET $2"));
    }

    #[test]
    fn list_sql_selects_all_entry_columns() {
        for col in &[
            "id",
            "action",
            "resource_type",
            "resource_id",
            "details",
            "admin_ip",
            "created_at",
        ] {
            assert!(
                LIST_ADMIN_AUDIT_LOG_SQL.contains(col),
                "missing column in SELECT: {col}"
            );
        }
        assert!(LIST_ADMIN_AUDIT_LOG_SQL.starts_with("SELECT"));
        assert!(LIST_ADMIN_AUDIT_LOG_SQL.contains("FROM admin_audit_log"));
    }

    #[test]
    fn insert_and_list_sql_use_sequential_placeholders_only() {
        for sql in [INSERT_ADMIN_AUDIT_LOG_SQL, LIST_ADMIN_AUDIT_LOG_SQL] {
            assert!(
                !sql.contains("$0"),
                "postgres placeholders are 1-indexed: {sql}"
            );
            assert!(
                !sql.contains(';'),
                "sqlx statements should not include trailing semicolons: {sql}"
            );
        }
    }

    // ---- Database error mapping (no live Postgres required) ---------------

    #[tokio::test]
    async fn log_admin_action_maps_execute_error_to_database() {
        let pool = unreachable_postgres_pool();
        let err = log_admin_action(&pool, "create", "tenant", "t-1", None, None)
            .await
            .unwrap_err();
        matches::assert_matches!(err, AppError::Database(msg) if !msg.is_empty());
    }

    #[tokio::test]
    async fn list_audit_log_maps_fetch_error_to_database() {
        let pool = unreachable_postgres_pool();
        let err = list_audit_log(&pool, 10, 0).await.unwrap_err();
        matches::assert_matches!(err, AppError::Database(msg) if !msg.is_empty());
    }
}
