//! Pure SQL string builders and row-to-domain conversions for PostgreSQL modules.
//!
//! These functions construct query text without executing against a database.

use ares_types::types::MessageRole;

// -----------------------------------------------------------------------------
// Alerts
// -----------------------------------------------------------------------------

pub const CREATE_ALERT_SQL: &str = "\
INSERT INTO alerts (id, severity, source, title, message, resolved, created_at) \
VALUES ($1, $2, $3, $4, $5, FALSE, $6)";

pub const RESOLVE_ALERT_SQL: &str = "\
UPDATE alerts SET resolved = TRUE, resolved_at = $1, resolved_by = $2 \
WHERE id = $3 AND resolved = FALSE";

pub const ACTIVE_ALERT_COUNT_SQL: &str =
    "SELECT COUNT(*) as cnt FROM alerts WHERE resolved = FALSE";

const LIST_ALERTS_BASE: &str = "\
SELECT id, severity, source, title, message, resolved, created_at, resolved_at, resolved_by \
FROM alerts WHERE 1=1";

/// Builds a parameterized-style filter query (used for logging / dynamic SQL tooling).
pub fn build_list_alerts_sql(
    severity_filter: Option<&str>,
    resolved_filter: Option<bool>,
    limit: i64,
) -> String {
    let mut query = String::from(LIST_ALERTS_BASE);
    let mut bind_idx = 1;

    if severity_filter.is_some() {
        query.push_str(&format!(" AND severity = ${}", bind_idx));
        bind_idx += 1;
    }
    if resolved_filter.is_some() {
        query.push_str(&format!(" AND resolved = ${}", bind_idx));
        let _ = bind_idx + 1;
    }

    query.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));
    query
}

/// Static SELECT used by `list_alerts` for each filter combination.
pub fn list_alerts_select_sql(
    severity_filter: Option<&str>,
    resolved_filter: Option<bool>,
) -> &'static str {
    match (severity_filter, resolved_filter) {
        (Some(_), Some(_)) => {
            "\
SELECT id, severity, source, title, message, resolved, created_at, resolved_at, resolved_by \
FROM alerts WHERE severity = $1 AND resolved = $2 ORDER BY created_at DESC LIMIT $3"
        }
        (Some(_), None) => {
            "\
SELECT id, severity, source, title, message, resolved, created_at, resolved_at, resolved_by \
FROM alerts WHERE severity = $1 ORDER BY created_at DESC LIMIT $2"
        }
        (None, Some(_)) => {
            "\
SELECT id, severity, source, title, message, resolved, created_at, resolved_at, resolved_by \
FROM alerts WHERE resolved = $1 ORDER BY created_at DESC LIMIT $2"
        }
        (None, None) => {
            "\
SELECT id, severity, source, title, message, resolved, created_at, resolved_at, resolved_by \
FROM alerts ORDER BY created_at DESC LIMIT $1"
        }
    }
}

// -----------------------------------------------------------------------------
// Agent config versions
// -----------------------------------------------------------------------------

pub const AGENT_VERSION_HISTORY_SQL: &str = "\
SELECT id, agent_id, version, config_json, is_active, change_source, created_at \
FROM agent_config_versions WHERE agent_id = $1 ORDER BY created_at DESC LIMIT $2";

/// Upsert SQL for `agent_config_versions` — rollback updates on conflict, others skip duplicates.
pub fn agent_version_upsert_sql(change_source: &str) -> &'static str {
    if change_source == "rollback" {
        "INSERT INTO agent_config_versions \
         (agent_id, version, config_json, is_active, change_source) \
         VALUES ($1, $2, $3, true, $4) \
         ON CONFLICT (agent_id, version) DO UPDATE \
         SET change_source = EXCLUDED.change_source, is_active = true"
    } else {
        "INSERT INTO agent_config_versions \
         (agent_id, version, config_json, is_active, change_source) \
         VALUES ($1, $2, $3, true, $4) \
         ON CONFLICT (agent_id, version) DO NOTHING"
    }
}

// -----------------------------------------------------------------------------
// Sessions / messages
// -----------------------------------------------------------------------------

pub const VALIDATE_SESSION_SQL: &str =
    "SELECT user_id FROM sessions WHERE token_hash = $1 AND expires_at > $2";

pub const DELETE_SESSION_BY_ID_SQL: &str = "DELETE FROM sessions WHERE id = $1";

pub const DELETE_SESSION_BY_TOKEN_SQL: &str = "DELETE FROM sessions WHERE token_hash = $1";

pub const INSERT_MESSAGE_SQL: &str = "\
INSERT INTO messages (id, conversation_id, role, content, timestamp) \
VALUES ($1, $2, $3, $4, $5)";

pub const SELECT_MESSAGES_SQL: &str = "\
SELECT role, content, timestamp FROM messages \
WHERE conversation_id = $1 ORDER BY timestamp ASC";

pub fn message_role_to_db(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

pub fn message_role_from_db(role: &str) -> MessageRole {
    match role {
        "system" => MessageRole::System,
        "assistant" => MessageRole::Assistant,
        _ => MessageRole::User,
    }
}

// -----------------------------------------------------------------------------
// Tenant agents
// -----------------------------------------------------------------------------

pub const DELETE_TENANT_AGENT_SQL: &str =
    "DELETE FROM tenant_agents WHERE tenant_id = $1 AND agent_name = $2";

pub const INSERT_TENANT_AGENT_SQL: &str = "\
INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at) \
VALUES ($1, $2, $3, $4, $5, $6, true, $7, $7)";

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts PostgreSQL-style `$n` bind placeholders in generated SQL.
    fn count_bind_placeholders(sql: &str) -> usize {
        let mut max_idx = 0usize;
        let mut i = 0;
        let bytes = sql.as_bytes();
        while i < bytes.len() {
            if bytes[i] == b'$' {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > start {
                    if let Ok(n) = sql[start..end].parse::<usize>() {
                        max_idx = max_idx.max(n);
                    }
                    i = end;
                    continue;
                }
            }
            i += 1;
        }
        max_idx
    }

    #[test]
    fn count_bind_placeholders_skips_non_numeric_dollar_signs() {
        assert_eq!(
            count_bind_placeholders("SELECT $x FROM alerts WHERE id = $1"),
            1
        );
        assert_eq!(count_bind_placeholders("no $ placeholders here"), 0);
    }

    #[test]
    fn count_bind_placeholders_tracks_max_index_not_count() {
        assert_eq!(count_bind_placeholders("WHERE parent = $1 AND id = $3"), 3);
    }

    #[test]
    fn create_alert_sql_inserts_with_six_bind_params() {
        assert!(CREATE_ALERT_SQL.starts_with("INSERT INTO alerts"));
        assert!(CREATE_ALERT_SQL.contains("resolved, created_at"));
        assert!(CREATE_ALERT_SQL.contains("FALSE"));
        assert_eq!(count_bind_placeholders(CREATE_ALERT_SQL), 6);
    }

    #[test]
    fn resolve_alert_sql_requires_unresolved_row_and_three_binds() {
        assert!(RESOLVE_ALERT_SQL.contains("resolved = TRUE"));
        assert!(RESOLVE_ALERT_SQL.contains("resolved_at = $1"));
        assert!(RESOLVE_ALERT_SQL.contains("resolved_by = $2"));
        assert!(RESOLVE_ALERT_SQL.contains("WHERE id = $3 AND resolved = FALSE"));
        assert_eq!(count_bind_placeholders(RESOLVE_ALERT_SQL), 3);
    }

    #[test]
    fn active_alert_count_sql_has_no_bind_params() {
        assert!(ACTIVE_ALERT_COUNT_SQL.contains("COUNT(*)"));
        assert!(ACTIVE_ALERT_COUNT_SQL.contains("resolved = FALSE"));
        assert_eq!(count_bind_placeholders(ACTIVE_ALERT_COUNT_SQL), 0);
    }

    #[test]
    fn build_list_alerts_sql_starts_from_dynamic_base() {
        let sql = build_list_alerts_sql(None, None, 5);
        assert!(sql.contains("FROM alerts WHERE 1=1"));
        assert!(sql.contains("ORDER BY created_at DESC LIMIT 5"));
        assert!(!sql.contains('$'));
    }

    #[test]
    fn build_list_alerts_sql_adds_filters_in_bind_order() {
        let sql = build_list_alerts_sql(Some("critical"), Some(true), 25);
        assert!(sql.contains("severity = $1"));
        assert!(sql.contains("resolved = $2"));
        assert!(sql.contains("LIMIT 25"));
        assert_eq!(count_bind_placeholders(&sql), 2);
    }

    #[test]
    fn build_list_alerts_sql_severity_only_uses_first_bind_slot() {
        let sql = build_list_alerts_sql(Some("warn"), None, 50);
        assert!(sql.contains("severity = $1"));
        assert!(!sql.contains("resolved ="));
        assert_eq!(count_bind_placeholders(&sql), 1);
    }

    #[test]
    fn build_list_alerts_sql_resolved_only_uses_first_bind_slot() {
        let sql = build_list_alerts_sql(None, Some(false), 15);
        assert!(sql.contains("resolved = $1"));
        assert!(!sql.contains("severity ="));
        assert_eq!(count_bind_placeholders(&sql), 1);
    }

    #[test]
    fn list_alerts_select_sql_covers_all_filter_branches() {
        let both = list_alerts_select_sql(Some("warn"), Some(false));
        assert!(both.contains("severity = $1 AND resolved = $2"));
        assert!(both.contains("LIMIT $3"));
        assert_eq!(count_bind_placeholders(both), 3);

        let severity_only = list_alerts_select_sql(Some("warn"), None);
        assert!(severity_only.contains("severity = $1"));
        assert!(!severity_only.contains("resolved ="));
        assert!(severity_only.contains("LIMIT $2"));
        assert_eq!(count_bind_placeholders(severity_only), 2);

        let resolved_only = list_alerts_select_sql(None, Some(true));
        assert!(resolved_only.contains("resolved = $1"));
        assert!(!resolved_only.contains("severity ="));
        assert!(resolved_only.contains("LIMIT $2"));
        assert_eq!(count_bind_placeholders(resolved_only), 2);

        let neither = list_alerts_select_sql(None, None);
        assert!(!neither.contains("WHERE severity"));
        assert!(!neither.contains("WHERE resolved"));
        assert!(neither.contains("LIMIT $1"));
        assert_eq!(count_bind_placeholders(neither), 1);
    }

    #[test]
    fn agent_version_history_sql_binds_agent_and_limit() {
        assert!(AGENT_VERSION_HISTORY_SQL.contains("agent_id = $1"));
        assert!(AGENT_VERSION_HISTORY_SQL.contains("LIMIT $2"));
        assert_eq!(count_bind_placeholders(AGENT_VERSION_HISTORY_SQL), 2);
    }

    #[test]
    fn agent_version_upsert_sql_rollback_updates_on_conflict() {
        let sql = agent_version_upsert_sql("rollback");
        assert!(sql.contains("VALUES ($1, $2, $3, true, $4)"));
        assert!(sql.contains("DO UPDATE"));
        assert!(sql.contains("is_active = true"));
        assert_eq!(count_bind_placeholders(sql), 4);
    }

    #[test]
    fn agent_version_upsert_sql_non_rollback_is_idempotent() {
        for source in ["startup", "hot_reload", "manual"] {
            let sql = agent_version_upsert_sql(source);
            assert!(sql.contains("DO NOTHING"), "source={source}");
            assert!(!sql.contains("DO UPDATE"), "source={source}");
            assert_eq!(count_bind_placeholders(sql), 4, "source={source}");
        }
    }

    #[test]
    fn session_sql_constants_use_expected_placeholders() {
        assert!(VALIDATE_SESSION_SQL.contains("token_hash = $1"));
        assert!(VALIDATE_SESSION_SQL.contains("expires_at > $2"));
        assert_eq!(count_bind_placeholders(VALIDATE_SESSION_SQL), 2);

        assert_eq!(
            DELETE_SESSION_BY_ID_SQL,
            "DELETE FROM sessions WHERE id = $1"
        );
        assert_eq!(
            DELETE_SESSION_BY_TOKEN_SQL,
            "DELETE FROM sessions WHERE token_hash = $1"
        );
    }

    #[test]
    fn message_sql_constants_bind_conversation_and_fields() {
        assert!(INSERT_MESSAGE_SQL.contains("INSERT INTO messages"));
        assert!(INSERT_MESSAGE_SQL.contains("VALUES ($1, $2, $3, $4, $5)"));
        assert_eq!(count_bind_placeholders(INSERT_MESSAGE_SQL), 5);

        assert!(SELECT_MESSAGES_SQL.contains("conversation_id = $1"));
        assert!(SELECT_MESSAGES_SQL.contains("ORDER BY timestamp ASC"));
        assert_eq!(count_bind_placeholders(SELECT_MESSAGES_SQL), 1);
    }

    #[test]
    fn message_role_round_trip() {
        let cases = [
            (MessageRole::System, "system"),
            (MessageRole::User, "user"),
            (MessageRole::Assistant, "assistant"),
        ];
        for (role, expected) in cases {
            assert_eq!(message_role_to_db(&role), expected);
            assert!(matches!(
                (role, message_role_from_db(expected)),
                (MessageRole::System, MessageRole::System)
                    | (MessageRole::User, MessageRole::User)
                    | (MessageRole::Assistant, MessageRole::Assistant)
            ));
        }
    }

    #[test]
    fn message_role_from_db_defaults_unknown_to_user() {
        assert!(matches!(message_role_from_db("human"), MessageRole::User));
        assert!(matches!(message_role_from_db(""), MessageRole::User));
    }

    #[test]
    fn tenant_agent_sql_targets_tenant_scope_with_expected_binds() {
        assert!(DELETE_TENANT_AGENT_SQL.contains("tenant_id = $1"));
        assert!(DELETE_TENANT_AGENT_SQL.contains("agent_name = $2"));
        assert_eq!(count_bind_placeholders(DELETE_TENANT_AGENT_SQL), 2);

        assert!(INSERT_TENANT_AGENT_SQL.contains("$7, $7"));
        assert!(!INSERT_TENANT_AGENT_SQL.contains("ON CONFLICT"));
        assert_eq!(count_bind_placeholders(INSERT_TENANT_AGENT_SQL), 7);
    }
}
