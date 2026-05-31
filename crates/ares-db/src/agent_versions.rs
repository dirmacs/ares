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

use ares_config::toon_config::ToonAgentConfig;

use crate::query_builders::{agent_version_upsert_sql, AGENT_VERSION_HISTORY_SQL};

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

        // For rollback events we need the row to be updated even if the version
        // already exists (so the rollback is durably recorded). For startup /
        // hot-reload we keep DO NOTHING to avoid noisy duplicates.
        let sql = agent_version_upsert_sql(change_source);

        match sqlx::query(sql)
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
    // Runtime `query_as` (not the `query_as!` macro) — same reason as
    // usage.rs: library crates shipped via crates.io cannot assume a
    // DATABASE_URL env var or `.sqlx` cache at downstream compile time.
    let rows = sqlx::query_as::<_, AgentVersionRecord>(AGENT_VERSION_HISTORY_SQL)
    .bind(agent_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// A row from agent_config_versions
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct AgentVersionRecord {
    pub id: String,
    pub agent_id: String,
    pub version: String,
    pub config_json: serde_json::Value,
    pub is_active: bool,
    pub change_source: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}


#[cfg(test)]
mod version_tests {
    use super::AgentVersionRecord;

    /// Helper: build a record with overridable fields for concise test setup.
    fn make_record(
        agent_id: &str,
        version: &str,
        config_json: serde_json::Value,
        is_active: bool,
        change_source: &str,
    ) -> AgentVersionRecord {
        AgentVersionRecord {
            id: "test-id".into(),
            agent_id: agent_id.into(),
            version: version.into(),
            config_json,
            is_active,
            change_source: change_source.into(),
            created_at: chrono::DateTime::parse_from_rfc3339("2025-01-15T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        }
    }

    // ── Serialization ──────────────────────────────────────────────

    #[test]
    fn serialize_preserves_all_string_fields() {
        let record = make_record(
            "agent-alpha",
            "2.3.1",
            serde_json::json!({"model": "fast"}),
            true,
            "hot_reload",
        );
        let json = serde_json::to_value(&record).unwrap();

        assert_eq!(json["id"], "test-id");
        assert_eq!(json["agent_id"], "agent-alpha");
        assert_eq!(json["version"], "2.3.1");
        assert_eq!(json["change_source"], "hot_reload");
    }

    #[test]
    fn serialize_boolean_is_active_true() {
        let record = make_record(
            "a",
            "1.0.0",
            serde_json::json!({}),
            true,
            "startup",
        );
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["is_active"], true);
    }

    #[test]
    fn serialize_boolean_is_active_false() {
        let record = make_record(
            "a",
            "1.0.0",
            serde_json::json!({}),
            false,
            "rollback",
        );
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["is_active"], false);
    }

    #[test]
    fn serialize_created_at_is_rfc3339() {
        let record = make_record(
            "a",
            "1.0.0",
            serde_json::json!({}),
            true,
            "startup",
        );
        let json = serde_json::to_value(&record).unwrap();
        let ts = json["created_at"].as_str().unwrap();
        // chrono serializes to RFC 3339 / ISO 8601
        assert!(ts.contains("2025-01-15"), "expected date in timestamp, got: {ts}");
        assert!(ts.contains("T"), "expected T separator in timestamp, got: {ts}");
    }

    // ── config_json shapes ─────────────────────────────────────────

    #[test]
    fn serialize_config_json_nested_object() {
        let config = serde_json::json!({
            "llm": { "provider": "openai", "model": "gpt-4" },
            "tools": ["search", "calculator"],
            "max_tokens": 4096,
        });
        let record = make_record("a", "1.0.0", config.clone(), true, "startup");
        let json = serde_json::to_value(&record).unwrap();

        assert_eq!(json["config_json"]["llm"]["provider"], "openai");
        assert_eq!(json["config_json"]["tools"][0], "search");
        assert_eq!(json["config_json"]["max_tokens"], 4096);
    }

    #[test]
    fn serialize_config_json_array() {
        let config = serde_json::json!([1, 2, 3]);
        let record = make_record("a", "1.0.0", config, true, "startup");
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["config_json"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn serialize_config_json_null() {
        let record = make_record("a", "1.0.0", serde_json::Value::Null, true, "startup");
        let json = serde_json::to_value(&record).unwrap();
        assert!(json["config_json"].is_null());
    }

    #[test]
    fn serialize_config_json_empty_object() {
        let record = make_record("a", "1.0.0", serde_json::json!({}), true, "startup");
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["config_json"], serde_json::json!({}));
    }

    // ── Edge cases: string values ──────────────────────────────────

    #[test]
    fn serialize_empty_agent_id() {
        let record = make_record("", "1.0.0", serde_json::json!({}), true, "startup");
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["agent_id"], "");
    }

    #[test]
    fn serialize_empty_version() {
        let record = make_record("a", "", serde_json::json!({}), true, "startup");
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["version"], "");
    }

    #[test]
    fn serialize_special_characters_in_agent_id() {
        let record = make_record(
            "agent/with-special_chars.v2",
            "1.0.0",
            serde_json::json!({}),
            true,
            "startup",
        );
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["agent_id"], "agent/with-special_chars.v2");
    }

    #[test]
    fn serialize_unicode_in_config_json() {
        let config = serde_json::json!({"prompt": "你好世界 🌍"});
        let record = make_record("a", "1.0.0", config, true, "startup");
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["config_json"]["prompt"], "你好世界 🌍");
    }

    // ── Clone ──────────────────────────────────────────────────────

    #[test]
    fn clone_is_independent() {
        let original = make_record(
            "agent-x",
            "1.0.0",
            serde_json::json!({"val": 1}),
            true,
            "startup",
        );
        let mut cloned = original.clone();
        cloned.agent_id = "mutated".into();
        cloned.version = "9.9.9".into();
        cloned.is_active = false;
        cloned.config_json = serde_json::json!({"val": 999});

        // Original must be untouched
        assert_eq!(original.agent_id, "agent-x");
        assert_eq!(original.version, "1.0.0");
        assert!(original.is_active);
        assert_eq!(original.config_json["val"], 1);
    }

    #[test]
    fn clone_matches_original_when_unchanged() {
        let original = make_record(
            "agent-x",
            "3.0.0",
            serde_json::json!({"k": "v"}),
            false,
            "rollback",
        );
        let cloned = original.clone();
        assert_eq!(original.id, cloned.id);
        assert_eq!(original.agent_id, cloned.agent_id);
        assert_eq!(original.version, cloned.version);
        assert_eq!(original.config_json, cloned.config_json);
        assert_eq!(original.is_active, cloned.is_active);
        assert_eq!(original.change_source, cloned.change_source);
    }

    // ── Debug ──────────────────────────────────────────────────────

    #[test]
    fn debug_contains_agent_id() {
        let record = make_record(
            "debug-agent",
            "0.1.0",
            serde_json::json!({}),
            true,
            "startup",
        );
        let dbg = format!("{:?}", record);
        assert!(dbg.contains("debug-agent"), "Debug output: {dbg}");
    }

    #[test]
    fn debug_contains_version() {
        let record = make_record("a", "4.2.0", serde_json::json!({}), true, "startup");
        let dbg = format!("{:?}", record);
        assert!(dbg.contains("4.2.0"), "Debug output: {dbg}");
    }

    // ── Version string variants ────────────────────────────────────

    #[test]
    fn serialize_semver_version_string() {
        let record = make_record(
            "a",
            "1.2.3-beta.1+build.456",
            serde_json::json!({}),
            true,
            "startup",
        );
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["version"], "1.2.3-beta.1+build.456");
    }

    #[test]
    fn serialize_hash_version_string() {
        let record = make_record(
            "a",
            "a1b2c3d4e5f6",
            serde_json::json!({}),
            true,
            "hot_reload",
        );
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["version"], "a1b2c3d4e5f6");
    }

    // ── Multiple records ───────────────────────────────────────────

    #[test]
    fn multiple_records_serialize_independently() {
        let r1 = make_record("agent-1", "1.0.0", serde_json::json!({"a": 1}), true, "startup");
        let r2 = make_record("agent-2", "2.0.0", serde_json::json!({"b": 2}), false, "rollback");

        let j1 = serde_json::to_value(&r1).unwrap();
        let j2 = serde_json::to_value(&r2).unwrap();

        assert_eq!(j1["agent_id"], "agent-1");
        assert_eq!(j1["version"], "1.0.0");
        assert_eq!(j1["is_active"], true);
        assert_eq!(j1["config_json"]["a"], 1);

        assert_eq!(j2["agent_id"], "agent-2");
        assert_eq!(j2["version"], "2.0.0");
        assert_eq!(j2["is_active"], false);
        assert_eq!(j2["config_json"]["b"], 2);
    }

    #[test]
    fn serialized_json_has_all_seven_fields() {
        let record = make_record("a", "1.0.0", serde_json::json!({}), true, "startup");
        let json = serde_json::to_value(&record).unwrap();
        let map = json.as_object().unwrap();
        assert_eq!(map.len(), 7, "AgentVersionRecord should have 7 fields");

        let expected_keys: std::collections::HashSet<&str> = [
            "id", "agent_id", "version", "config_json",
            "is_active", "change_source", "created_at",
        ]
        .into_iter()
        .collect();
        let actual_keys: std::collections::HashSet<&str> =
            map.keys().map(|k| k.as_str()).collect();
        assert_eq!(actual_keys, expected_keys);
    }

    // ── Deep nesting stress ────────────────────────────────────────

    #[test]
    fn serialize_deeply_nested_config_json() {
        let config = serde_json::json!({
            "level1": {
                "level2": {
                    "level3": {
                        "level4": {
                            "value": "deep"
                        }
                    }
                }
            }
        });
        let record = make_record("a", "1.0.0", config, true, "startup");
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(
            json["config_json"]["level1"]["level2"]["level3"]["level4"]["value"],
            "deep"
        );
    }
}
