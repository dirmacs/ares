use crate::query_builders::{DELETE_TENANT_AGENT_SQL, INSERT_TENANT_AGENT_SQL};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantAgent {
    pub id: String,
    pub tenant_id: String,
    pub agent_name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTemplate {
    pub id: String,
    pub product_type: String,
    pub agent_name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub config: serde_json::Value,
    pub created_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateTenantAgentRequest {
    pub agent_name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub config: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTenantAgentRequest {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub config: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

fn row_to_tenant_agent(row: &sqlx::postgres::PgRow) -> TenantAgent {
    TenantAgent {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        agent_name: row.get("agent_name"),
        display_name: row.get("display_name"),
        description: row.get("description"),
        config: row.get::<serde_json::Value, _>("config"),
        enabled: row.get("enabled"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

pub fn tenant_agent_version_key(tenant_id: &str, agent_name: &str) -> String {
    format!("tenant:{}:{}", tenant_id, agent_name)
}

fn active_runtime_config_version(agent: &TenantAgent) -> String {
    agent
        .config
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .unwrap_or_else(|| format!("tenant-db:{}", agent.updated_at))
}

fn tenant_agent_snapshot_version(agent: &TenantAgent) -> String {
    format!("tenant-db:{}:{}", agent.updated_at, uuid::Uuid::new_v4())
}

fn tenant_agent_snapshot(agent: &TenantAgent) -> serde_json::Value {
    serde_json::json!({
        "snapshot_type": "tenant_agent",
        "runtime_config_version": active_runtime_config_version(agent),
        "tenant_agent": agent,
    })
}

fn tenant_agent_from_snapshot(snapshot: serde_json::Value) -> Result<TenantAgent> {
    let value = if let Some(agent) = snapshot.get("tenant_agent") {
        agent.clone()
    } else {
        snapshot
    };

    serde_json::from_value(value).map_err(|e| {
        AppError::InvalidInput(format!(
            "Failed to deserialize tenant agent snapshot: {}",
            e
        ))
    })
}

pub async fn record_tenant_agent_version(
    pool: &PgPool,
    agent: &TenantAgent,
    change_source: &str,
) -> Result<crate::agent_versions::AgentVersionRecord> {
    let agent_id = tenant_agent_version_key(&agent.tenant_id, &agent.agent_name);
    let version = tenant_agent_snapshot_version(agent);
    let config_json = tenant_agent_snapshot(agent);

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    sqlx::query("UPDATE agent_config_versions SET is_active = false WHERE agent_id = $1")
        .bind(&agent_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    let record = sqlx::query_as::<_, crate::agent_versions::AgentVersionRecord>(
        r#"INSERT INTO agent_config_versions
           (agent_id, version, config_json, is_active, change_source)
           VALUES ($1, $2, $3, true, $4)
           RETURNING id, agent_id, version, config_json, is_active, change_source, created_at"#,
    )
    .bind(&agent_id)
    .bind(&version)
    .bind(&config_json)
    .bind(change_source)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(record)
}

pub async fn list_tenant_agent_versions(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
    limit: i64,
) -> Result<Vec<crate::agent_versions::AgentVersionRecord>> {
    let agent_id = tenant_agent_version_key(tenant_id, agent_name);
    let rows = sqlx::query_as::<_, crate::agent_versions::AgentVersionRecord>(
        r#"SELECT id, agent_id, version, config_json, is_active, change_source, created_at
           FROM agent_config_versions
           WHERE agent_id = $1
           ORDER BY created_at DESC
           LIMIT $2"#,
    )
    .bind(&agent_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(rows)
}

pub async fn rollback_tenant_agent_version(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
    version: &str,
) -> Result<TenantAgent> {
    let agent_id = tenant_agent_version_key(tenant_id, agent_name);
    let record = sqlx::query(
        "SELECT config_json FROM agent_config_versions WHERE agent_id = $1 AND version = $2",
    )
    .bind(&agent_id)
    .bind(version)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?
    .ok_or_else(|| {
        AppError::NotFound(format!(
            "No version '{}' found for tenant agent '{}'",
            version, agent_id
        ))
    })?;

    let snapshot = tenant_agent_from_snapshot(record.get("config_json"))?;
    if snapshot.tenant_id != tenant_id || snapshot.agent_name != agent_name {
        return Err(AppError::InvalidInput(format!(
            "Version '{}' does not belong to tenant agent '{}'",
            version, agent_id
        )));
    }

    let now = now_ts();
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    let row = sqlx::query(
        r#"UPDATE tenant_agents
           SET display_name = $1, description = $2, config = $3, enabled = $4, updated_at = $5
           WHERE tenant_id = $6 AND agent_name = $7
           RETURNING id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at"#,
    )
    .bind(&snapshot.display_name)
    .bind(&snapshot.description)
    .bind(&snapshot.config)
    .bind(snapshot.enabled)
    .bind(now)
    .bind(tenant_id)
    .bind(agent_name)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?
    .ok_or_else(|| {
        AppError::NotFound(format!(
            "Agent '{}' not found for tenant '{}'",
            agent_name, tenant_id
        ))
    })?;

    let restored = row_to_tenant_agent(&row);
    let restored_version = tenant_agent_snapshot_version(&restored);
    let restored_snapshot = tenant_agent_snapshot(&restored);
    let change_source = format!("rollback:{}", version);

    sqlx::query("UPDATE agent_config_versions SET is_active = false WHERE agent_id = $1")
        .bind(&agent_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    sqlx::query(
        r#"INSERT INTO agent_config_versions
           (agent_id, version, config_json, is_active, change_source)
           VALUES ($1, $2, $3, true, $4)"#,
    )
    .bind(&agent_id)
    .bind(&restored_version)
    .bind(&restored_snapshot)
    .bind(&change_source)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(restored)
}

// =============================================================================
// Tenant Agent CRUD
// =============================================================================

pub async fn list_tenant_agents(pool: &PgPool, tenant_id: &str) -> Result<Vec<TenantAgent>> {
    let rows = sqlx::query(
        "SELECT id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at
         FROM tenant_agents WHERE tenant_id = $1 ORDER BY agent_name"
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(rows.iter().map(row_to_tenant_agent).collect())
}

pub async fn get_tenant_agent(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
) -> Result<TenantAgent> {
    let row = sqlx::query(
        "SELECT id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at
         FROM tenant_agents WHERE tenant_id = $1 AND agent_name = $2"
    )
    .bind(tenant_id)
    .bind(agent_name)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?
    .ok_or_else(|| AppError::NotFound(format!("Agent '{}' not found for tenant '{}'", agent_name, tenant_id)))?;

    Ok(row_to_tenant_agent(&row))
}

pub async fn create_tenant_agent(
    pool: &PgPool,
    tenant_id: &str,
    req: CreateTenantAgentRequest,
) -> Result<TenantAgent> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ts();

    sqlx::query(INSERT_TENANT_AGENT_SQL)
    .bind(&id)
    .bind(tenant_id)
    .bind(&req.agent_name)
    .bind(&req.display_name)
    .bind(&req.description)
    .bind(&req.config)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    let agent = get_tenant_agent(pool, tenant_id, &req.agent_name).await?;
    record_tenant_agent_version(pool, &agent, "admin_create").await?;
    Ok(agent)
}

pub async fn update_tenant_agent(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
    req: UpdateTenantAgentRequest,
) -> Result<TenantAgent> {
    let now = now_ts();

    // Fetch current state
    let current = get_tenant_agent(pool, tenant_id, agent_name).await?;

    let display_name = req.display_name.unwrap_or(current.display_name);
    let description = req.description.or(current.description);
    let config = req.config.unwrap_or(current.config);
    let enabled = req.enabled.unwrap_or(current.enabled);

    sqlx::query(
        "UPDATE tenant_agents SET display_name = $1, description = $2, config = $3, enabled = $4, updated_at = $5
         WHERE tenant_id = $6 AND agent_name = $7"
    )
    .bind(&display_name)
    .bind(&description)
    .bind(&config)
    .bind(enabled)
    .bind(now)
    .bind(tenant_id)
    .bind(agent_name)
    .execute(pool)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    let agent = get_tenant_agent(pool, tenant_id, agent_name).await?;
    record_tenant_agent_version(pool, &agent, "admin_update").await?;
    Ok(agent)
}

pub async fn delete_tenant_agent(pool: &PgPool, tenant_id: &str, agent_name: &str) -> Result<()> {
    let result = sqlx::query(DELETE_TENANT_AGENT_SQL)
        .bind(tenant_id)
        .bind(agent_name)
        .execute(pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "Agent '{}' not found for tenant '{}'",
            agent_name, tenant_id
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_agent() -> TenantAgent {
        TenantAgent {
            id: "agent-row-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            agent_name: "listener".to_string(),
            display_name: "Listener".to_string(),
            description: Some("desc".to_string()),
            config: serde_json::json!({
                "model": "fast",
                "system_prompt": "test",
            }),
            enabled: true,
            created_at: 10,
            updated_at: 20,
        }
    }

    fn sample_agent_with_version() -> TenantAgent {
        TenantAgent {
            id: "agent-row-2".to_string(),
            tenant_id: "tenant-1".to_string(),
            agent_name: "writer".to_string(),
            display_name: "Writer".to_string(),
            description: Some("a writer agent".to_string()),
            config: serde_json::json!({
                "model": "slow",
                "version": "v2.1.0",
            }),
            enabled: true,
            created_at: 100,
            updated_at: 200,
        }
    }

    // =====================================================================
    // Existing tests
    // =====================================================================

    #[test]
    fn tenant_version_key_is_tenant_scoped() {
        assert_eq!(
            tenant_agent_version_key("tenant-1", "listener"),
            "tenant:tenant-1:listener"
        );
    }

    #[test]
    fn tenant_agent_snapshot_round_trips() {
        let agent = sample_agent();
        let snapshot = tenant_agent_snapshot(&agent);
        assert_eq!(
            snapshot["runtime_config_version"].as_str(),
            Some("tenant-db:20")
        );

        let restored = tenant_agent_from_snapshot(snapshot).expect("snapshot should deserialize");
        assert_eq!(restored.tenant_id, "tenant-1");
        assert_eq!(restored.agent_name, "listener");
        assert_eq!(restored.config["model"], "fast");
    }

    // =====================================================================
    // now_ts
    // =====================================================================

    #[test]
    fn now_ts_returns_positive_value_near_current_time() {
        let ts = now_ts();
        assert!(ts > 0, "now_ts should be positive, got {ts}");
        // Current unix time in 2026 is well above 1_700_000_000
        assert!(ts > 1_700_000_000, "now_ts seems too small: {ts}");
    }

    // =====================================================================
    // tenant_agent_version_key — additional cases
    // =====================================================================

    #[test]
    fn tenant_agent_version_key_empty_tenant() {
        assert_eq!(
            tenant_agent_version_key("", "agent"),
            "tenant::agent"
        );
    }

    #[test]
    fn tenant_agent_version_key_empty_agent() {
        assert_eq!(
            tenant_agent_version_key("tenant", ""),
            "tenant:tenant:"
        );
    }

    #[test]
    fn tenant_agent_version_key_special_chars() {
        assert_eq!(
            tenant_agent_version_key("t-1_2", "a/b:c"),
            "tenant:t-1_2:a/b:c"
        );
    }

    // =====================================================================
    // active_runtime_config_version
    // =====================================================================

    #[test]
    fn active_runtime_config_version_with_version_in_config() {
        let agent = sample_agent_with_version();
        assert_eq!(active_runtime_config_version(&agent), "v2.1.0");
    }

    #[test]
    fn active_runtime_config_version_without_version() {
        let agent = sample_agent();
        assert_eq!(
            active_runtime_config_version(&agent),
            "tenant-db:20"
        );
    }

    #[test]
    fn active_runtime_config_version_empty_string_falls_back() {
        let mut agent = sample_agent();
        agent.config = serde_json::json!({ "version": "" });
        assert_eq!(
            active_runtime_config_version(&agent),
            "tenant-db:20"
        );
    }

    #[test]
    fn active_runtime_config_version_whitespace_only_falls_back() {
        let mut agent = sample_agent();
        agent.config = serde_json::json!({ "version": "   " });
        assert_eq!(
            active_runtime_config_version(&agent),
            "tenant-db:20"
        );
    }

    // =====================================================================
    // tenant_agent_snapshot_version
    // =====================================================================

    #[test]
    fn tenant_agent_snapshot_version_format() {
        let agent = sample_agent();
        let ver = tenant_agent_snapshot_version(&agent);
        // Format: "tenant-db:{updated_at}:{uuid}"
        assert!(
            ver.starts_with("tenant-db:20:"),
            "expected prefix 'tenant-db:20:', got '{ver}'"
        );
        let suffix = ver.strip_prefix("tenant-db:20:").unwrap();
        // UUID v4 has format 8-4-4-4-12 hex chars
        assert_eq!(suffix.len(), 36, "UUID should be 36 chars, got '{}'", suffix);
        assert_eq!(
            suffix.chars().filter(|c| *c == '-').count(),
            4,
            "UUID should have 4 dashes"
        );
    }

    // =====================================================================
    // tenant_agent_from_snapshot — edge cases
    // =====================================================================

    #[test]
    fn tenant_agent_from_snapshot_with_nested_key() {
        let agent = sample_agent();
        let snapshot = tenant_agent_snapshot(&agent);
        assert_eq!(snapshot.get("tenant_agent").and_then(|v| v.get("agent_name")), Some(&serde_json::json!("listener")));

        let restored = tenant_agent_from_snapshot(snapshot).expect("should deserialize nested");
        assert_eq!(restored.agent_name, "listener");
    }

    #[test]
    fn tenant_agent_from_snapshot_without_nested_key() {
        let agent = sample_agent();
        let raw = serde_json::to_value(&agent).unwrap();
        let restored =
            tenant_agent_from_snapshot(raw).expect("should deserialize flat agent");
        assert_eq!(restored.agent_name, "listener");
        assert_eq!(restored.tenant_id, "tenant-1");
    }

    #[test]
    fn tenant_agent_from_snapshot_invalid_json() {
        let bad = serde_json::json!({ "completely": "wrong", "fields": 42 });
        let result = tenant_agent_from_snapshot(bad);
        assert!(result.is_err(), "should error on invalid snapshot");
    }

    // =====================================================================
    // tenant_agent_snapshot — structure
    // =====================================================================

    #[test]
    fn tenant_agent_snapshot_structure() {
        let agent = sample_agent_with_version();
        let snap = tenant_agent_snapshot(&agent);

        assert_eq!(snap["snapshot_type"].as_str(), Some("tenant_agent"));
        assert_eq!(
            snap["runtime_config_version"].as_str(),
            Some("v2.1.0")
        );
        // Nested agent has the same fields
        assert_eq!(snap["tenant_agent"]["agent_name"].as_str(), Some("writer"));
        assert_eq!(snap["tenant_agent"]["tenant_id"].as_str(), Some("tenant-1"));
        assert_eq!(snap["tenant_agent"]["enabled"].as_bool(), Some(true));
    }

    // =====================================================================
    // Struct serde tests
    // =====================================================================

    #[test]
    fn tenant_agent_serde_roundtrip() {
        let agent = sample_agent();
        let json = serde_json::to_string(&agent).expect("serialize");
        let restored: TenantAgent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.id, agent.id);
        assert_eq!(restored.tenant_id, agent.tenant_id);
        assert_eq!(restored.agent_name, agent.agent_name);
        assert_eq!(restored.enabled, agent.enabled);
        assert_eq!(restored.created_at, agent.created_at);
        assert_eq!(restored.updated_at, agent.updated_at);
        assert_eq!(restored.config, agent.config);
    }

    #[test]
    fn tenant_agent_debug_format_contains_key_fields() {
        let agent = sample_agent();
        let dbg = format!("{:?}", agent);
        assert!(dbg.contains("agent-row-1"), "Debug should contain id");
        assert!(dbg.contains("listener"), "Debug should contain agent_name");
        assert!(dbg.contains("tenant-1"), "Debug should contain tenant_id");
    }

    #[test]
    fn tenant_agent_clone_produces_equal_value() {
        let agent = sample_agent();
        let cloned = agent.clone();
        assert_eq!(agent.id, cloned.id);
        assert_eq!(agent.tenant_id, cloned.tenant_id);
        assert_eq!(agent.agent_name, cloned.agent_name);
        assert_eq!(agent.display_name, cloned.display_name);
        assert_eq!(agent.description, cloned.description);
        assert_eq!(agent.config, cloned.config);
        assert_eq!(agent.enabled, cloned.enabled);
        assert_eq!(agent.created_at, cloned.created_at);
        assert_eq!(agent.updated_at, cloned.updated_at);
    }

    #[test]
    fn agent_template_serde_roundtrip() {
        let tmpl = AgentTemplate {
            id: "tmpl-1".to_string(),
            product_type: "insurance".to_string(),
            agent_name: "claims".to_string(),
            display_name: "Claims Agent".to_string(),
            description: Some("handles claims".to_string()),
            config: serde_json::json!({ "model": "gpt-4" }),
            created_at: 500,
        };
        let json = serde_json::to_string(&tmpl).expect("serialize");
        let restored: AgentTemplate = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.id, tmpl.id);
        assert_eq!(restored.product_type, tmpl.product_type);
        assert_eq!(restored.config, tmpl.config);
    }

    #[test]
    fn create_tenant_agent_request_full() {
        let json = r#"{
            "agent_name": "qa-bot",
            "display_name": "QA Bot",
            "description": "quality assurance",
            "config": {"model": "fast"}
        }"#;
        let req: CreateTenantAgentRequest =
            serde_json::from_str(json).expect("deserialize full request");
        assert_eq!(req.agent_name, "qa-bot");
        assert_eq!(req.display_name, "QA Bot");
        assert_eq!(req.description, Some("quality assurance".to_string()));
        assert_eq!(req.config["model"], "fast");
    }

    #[test]
    fn create_tenant_agent_request_optional_fields_missing() {
        let json = r#"{
            "agent_name": "qa-bot",
            "display_name": "QA Bot",
            "config": {}
        }"#;
        let req: CreateTenantAgentRequest =
            serde_json::from_str(json).expect("deserialize without description");
        assert_eq!(req.agent_name, "qa-bot");
        assert!(req.description.is_none());
    }

    #[test]
    fn update_tenant_agent_request_all_fields() {
        let json = r#"{
            "display_name": "New Name",
            "description": "new desc",
            "config": {"key": "val"},
            "enabled": false
        }"#;
        let req: UpdateTenantAgentRequest =
            serde_json::from_str(json).expect("deserialize full update");
        assert_eq!(req.display_name.as_deref(), Some("New Name"));
        assert_eq!(req.description.as_deref(), Some("new desc"));
        assert!(req.config.is_some());
        assert_eq!(req.enabled, Some(false));
    }

    #[test]
    fn update_tenant_agent_request_no_fields() {
        let json = r#"{}"#;
        let req: UpdateTenantAgentRequest =
            serde_json::from_str(json).expect("deserialize empty update");
        assert!(req.display_name.is_none());
        assert!(req.description.is_none());
        assert!(req.config.is_none());
        assert!(req.enabled.is_none());
    }

    #[test]
    fn update_tenant_agent_request_partial_enabled_only() {
        let json = r#"{"enabled": true}"#;
        let req: UpdateTenantAgentRequest =
            serde_json::from_str(json).expect("deserialize partial update");
        assert!(req.display_name.is_none());
        assert!(req.description.is_none());
        assert!(req.config.is_none());
        assert_eq!(req.enabled, Some(true));
    }
}

// =============================================================================
// Template operations
// =============================================================================

pub async fn list_agent_templates(
    pool: &PgPool,
    product_type: Option<&str>,
) -> Result<Vec<AgentTemplate>> {
    let rows = if let Some(pt) = product_type {
        sqlx::query(
            "SELECT id, product_type, agent_name, display_name, description, config, created_at
             FROM agent_templates WHERE product_type = $1 ORDER BY agent_name",
        )
        .bind(pt)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?
    } else {
        sqlx::query(
            "SELECT id, product_type, agent_name, display_name, description, config, created_at
             FROM agent_templates ORDER BY product_type, agent_name",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?
    };

    rows.iter()
        .map(|row| {
            Ok(AgentTemplate {
                id: row.get("id"),
                product_type: row.get("product_type"),
                agent_name: row.get("agent_name"),
                display_name: row.get("display_name"),
                description: row.get("description"),
                config: row.get::<serde_json::Value, _>("config"),
                created_at: row.get("created_at"),
            })
        })
        .collect()
}

/// Clones all agent templates for a product type into a tenant's agent list.
/// Idempotent — skips agents that already exist (ON CONFLICT DO NOTHING).
pub async fn clone_templates_for_tenant(
    pool: &PgPool,
    tenant_id: &str,
    product_type: &str,
) -> Result<Vec<TenantAgent>> {
    let templates = list_agent_templates(pool, Some(product_type)).await?;
    let now = now_ts();

    for tpl in &templates {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, true, $7, $7)
             ON CONFLICT (tenant_id, agent_name) DO NOTHING"
        )
        .bind(&id)
        .bind(tenant_id)
        .bind(&tpl.agent_name)
        .bind(&tpl.display_name)
        .bind(&tpl.description)
        .bind(&tpl.config)
        .bind(now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;
    }

    list_tenant_agents(pool, tenant_id).await
}

// =============================================================================
// Seed default templates
// =============================================================================

/// Seeds default agent templates. Idempotent — uses ON CONFLICT DO NOTHING.
/// Called once on ARES startup after migrations.
pub async fn seed_default_templates(pool: &PgPool) -> Result<()> {
    let now = now_ts();

    struct TemplateSpec {
        product_type: &'static str,
        agent_name: &'static str,
        display_name: &'static str,
        description: &'static str,
        model: &'static str,
        system_prompt: &'static str,
    }

    let templates: &[TemplateSpec] = &[
        // Generic
        TemplateSpec {
            product_type: "generic",
            agent_name: "assistant",
            display_name: "General Assistant",
            description: "Default conversational agent",
            model: "fast",
            system_prompt: "You are a helpful AI assistant. Answer questions clearly and concisely. If you don't know something, say so. Be direct and useful.",
        },
        // Client-specific agent templates are loaded by the managed platform crate
        // from TOON config files, not hardcoded in the OSS layer.
    ];

    for tpl in templates {
        let id = uuid::Uuid::new_v4().to_string();
        let config = serde_json::json!({
            "model": tpl.model,
            "system_prompt": tpl.system_prompt,
            "tools": [],
            "max_tool_iterations": 3
        });

        sqlx::query(
            "INSERT INTO agent_templates (id, product_type, agent_name, display_name, description, config, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (product_type, agent_name) DO NOTHING"
        )
        .bind(&id)
        .bind(tpl.product_type)
        .bind(tpl.agent_name)
        .bind(tpl.display_name)
        .bind(tpl.description)
        .bind(&config)
        .bind(now)
        .execute(pool)
        .await
        .map_err(|e| AppError::Database(format!("Failed to seed template {}/{}: {}", tpl.product_type, tpl.agent_name, e)))?;
    }

    tracing::info!("Agent templates seeded ({} templates)", templates.len());
    Ok(())
}
